//! Pasted and dropped video receipts. A receipt check, Kettle itself run as
//! a short-lived helper (`__media-preview-worker`), opens the named file
//! through a trusted parent chain, identifies it, sees that its bytes start
//! like a video and decodes nothing; on Linux it also opens the video's
//! cached freedesktop.org thumbnails the same way. The poster then comes from
//! the sandboxed media worker, held to the inode the check opened: a frame of
//! the video, or on Linux, when no decoder can make one, a cached thumbnail
//! the worker shows is that video's. On Windows, where no media worker runs,
//! the check asks the Shell for the poster itself. A checked video with no
//! poster still gets a receipt, without one.

use std::ffi::OsString;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

/// The request frame: this magic, the parent's build identity, the path.
const INPUT_MAGIC: &[u8; 8] = b"KTLVPIN2";
/// What every version of the request frame starts with, so a worker from
/// another build can tell a skewed request from garbage.
const INPUT_MAGIC_FAMILY: &[u8; 7] = b"KTLVPIN";
const MAX_BUILD_IDENTITY_BYTES: usize = 128;
const OUTPUT_MAGIC: &[u8; 8] = b"KTLVPOU2";
const MAX_PATH_BYTES: usize = 64 * 1024;
const MAX_PREVIEW_WIDTH: u32 = 256;
const MAX_PREVIEW_HEIGHT: u32 = 160;
const MAX_PREVIEW_BYTES: usize = MAX_PREVIEW_WIDTH as usize * MAX_PREVIEW_HEIGHT as usize * 4;
const WORKER_TIMEOUT: Duration = Duration::from_secs(2);
const WORKER_TIMEOUT_EXIT: i32 = 4;
const WORKER_WATCHDOG_SETUP_EXIT: i32 = 8;
/// The worker is another build than its parent (an update replaced the
/// executable on disk): it refuses rather than answer in a protocol the
/// parent may not share.
const WORKER_SKEW_EXIT: i32 = 9;

static BUILD_IDENTITY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record the source this build came from (the version and a hash of the
/// Rust sources), which the preview worker must share with its parent. Set
/// once, at process start.
pub(crate) fn set_build_identity(identity: &str) {
    let mut identity = identity.to_owned();
    while identity.len() > MAX_BUILD_IDENTITY_BYTES {
        identity.pop();
    }
    let _ = BUILD_IDENTITY.set(identity);
}

fn build_identity() -> &'static str {
    BUILD_IDENTITY
        .get()
        .map_or(env!("CARGO_PKG_VERSION"), String::as_str)
}

/// Whether a request frame came from this build (see [`decode_request`]).
#[derive(Debug, Eq, PartialEq)]
enum RequestError {
    /// Another build's frame: an older or newer magic, or another identity.
    Skewed,
    Malformed,
}
const MAX_WORKER_ATTEMPTS: u32 = 2;
const PREVIEW_THREAD_COUNT: usize = 2;
const PREVIEW_QUEUE_CAPACITY: usize = 8;
/// How long a receipt has from its request for its checks and its poster:
/// no check or render starts after it, and each stops at it. A job that
/// waited it out in the queue gets no receipt. It holds both checks' two
/// attempts and the media worker's cold start, twice, and render.
const RECEIPT_DEADLINE: Duration = Duration::from_secs(20);
/// Pending state outlives the deadline by two seconds of cleanup and
/// dispatch slack, so an unusually loaded host drops the optional receipt
/// instead of keeping pending state.
pub(crate) const PENDING_RECEIPT_TIMEOUT: Duration =
    Duration::from_secs(RECEIPT_DEADLINE.as_secs() + 2);
const MAX_FILE_LIST_ENTRIES: usize = 256;
const FINGERPRINT_SAMPLE_BYTES: usize = 64 * 1024;
/// Most cached thumbnails a check names: one for each of the standard's sizes.
const MAX_CACHED_THUMBNAILS: usize = 4;
/// The longest reply a check sends: its fixed fields, every cached thumbnail
/// at the longest path, and the largest poster.
const MAX_OUTPUT_BYTES: usize = OUTPUT_MAGIC.len()
    + 8 * 4
    + 4
    + 32
    + 1
    + MAX_CACHED_THUMBNAILS * (8 + 8 + 4 + MAX_PATH_BYTES)
    + 4 * 3
    + MAX_PREVIEW_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VideoPasteSource {
    Clipboard,
    Drop,
}

/// Cheap path metadata captured on the event loop. Filesystem trust and
/// fingerprinting happen only after this enters the bounded preview queue.
#[derive(Clone, Debug)]
pub(crate) struct VideoPasteRequest {
    path: PathBuf,
    pub(crate) count: usize,
    pub(crate) extension: String,
    pub(crate) source: VideoPasteSource,
}

impl VideoPasteRequest {
    pub(crate) fn from_user_paths(paths: &[PathBuf], source: VideoPasteSource) -> Option<Self> {
        if paths.len() > MAX_FILE_LIST_ENTRIES {
            return None;
        }
        let mut first = None;
        let mut count = 0usize;
        for path in paths {
            let Some(extension) = video_extension(path) else {
                continue;
            };
            count += 1;
            if first.is_none() {
                first = Some((path.clone(), extension));
            }
        }
        let (path, extension) = first?;
        Some(Self {
            path,
            count,
            extension,
            source,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn merge_drop(&mut self, newer: &Self, elapsed: Duration) -> bool {
        if self.source != VideoPasteSource::Drop
            || newer.source != VideoPasteSource::Drop
            || newer.path == self.path
            || elapsed > Duration::from_millis(250)
        {
            return false;
        }
        self.count = self.count.saturating_add(newer.count);
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileIdentity {
    len: u64,
    modified: Option<SystemTime>,
    fingerprint: [u8; 32],
    platform: PlatformFileIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlatformFileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
    #[cfg(windows)]
    change_time: i64,
    #[cfg(windows)]
    write_time: i64,
}

#[derive(Clone, Debug)]
pub struct VideoPasteCandidate {
    path: PathBuf,
    pub(crate) count: usize,
    pub(crate) extension: String,
    pub(crate) size: u64,
    pub(crate) source: VideoPasteSource,
}

impl VideoPasteCandidate {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn from_verified(request: VideoPasteRequest, size: u64) -> Self {
        Self {
            path: request.path,
            size,
            count: request.count,
            extension: request.extension,
            source: request.source,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_user_paths(paths: &[PathBuf], source: VideoPasteSource) -> Option<Self> {
        let request = VideoPasteRequest::from_user_paths(paths, source)?;
        let (identity, _) = file_identity(request.path())?;
        Some(Self::from_verified(request, identity.len))
    }
}

fn file_identity_matches(
    path: &Path,
    identity: &FileIdentity,
    retained: &std::sync::Arc<std::fs::File>,
) -> bool {
    let held = retained.try_clone().ok().and_then(file_identity_from_open);
    held.as_ref() == Some(identity)
        && file_identity(path).is_some_and(|(current, _)| current == *identity)
}

fn explicit_video_extension(path: &Path) -> Option<String> {
    let extension = video_extension(path)?;
    let link = std::fs::symlink_metadata(path).ok()?;
    if !link.file_type().is_file() {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if link.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }
    }
    Some(extension)
}

fn video_extension(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    const VIDEO_EXTENSIONS: &[&str] = &[
        "3g2", "3gp", "avi", "flv", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "ogv",
        "webm", "wmv",
    ];
    debug_assert!(
        VIDEO_EXTENSIONS.windows(2).all(|pair| pair[0] < pair[1]),
        "video extensions must remain sorted for binary_search"
    );
    if VIDEO_EXTENSIONS.binary_search(&extension.as_str()).is_err() {
        return None;
    }
    Some(extension.to_ascii_uppercase())
}

fn file_identity(path: &Path) -> Option<(FileIdentity, std::sync::Arc<std::fs::File>)> {
    explicit_video_extension(path)?;
    // This is stricter than a no-follow leaf open. The held parent chain also
    // rejects directory permissions or ACLs that let another principal swap
    // the name while a native path-only thumbnail API is reading it.
    let file = kettle_state::open_trusted_file_read(path).ok()?;
    let identity = file.try_clone().ok().and_then(file_identity_from_open)?;
    Some((identity, std::sync::Arc::new(file)))
}

fn file_identity_from_open(mut file: std::fs::File) -> Option<FileIdentity> {
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some(FileIdentity {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        fingerprint: sampled_fingerprint(&mut file, metadata.len())?,
        platform: platform_file_identity(&file, &metadata)?,
    })
}

fn sampled_fingerprint(file: &mut std::fs::File, len: u64) -> Option<[u8; 32]> {
    use sha2::{Digest as _, Sha256};

    // `File::try_clone` may duplicate this handle while sharing its file
    // offset. Seek before every read so background validation cannot depend on
    // whichever clone read most recently.
    let sample = FINGERPRINT_SAMPLE_BYTES as u64;
    let middle = len
        .saturating_div(2)
        .saturating_sub(sample.saturating_div(2));
    let mut offsets = [0, middle, len.saturating_sub(sample)];
    offsets.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(len.to_le_bytes());
    let mut previous = None;
    for offset in offsets {
        if previous == Some(offset) {
            continue;
        }
        previous = Some(offset);
        let available = len.saturating_sub(offset).min(sample) as usize;
        let mut bytes = vec![0; available];
        file.seek(std::io::SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut bytes).ok()?;
        hasher.update(offset.to_le_bytes());
        hasher.update(&bytes);
    }
    Some(hasher.finalize().into())
}

#[cfg(unix)]
fn platform_file_identity(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Option<PlatformFileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    Some(PlatformFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn platform_file_identity(
    file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Option<PlatformFileIdentity> {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FILE_ID_INFO, FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut info = FILE_ID_INFO::default();
    let mut basic = FILE_BASIC_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
        .ok()?;
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            std::ptr::addr_of_mut!(basic).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
        .ok()?;
    }
    Some(PlatformFileIdentity {
        volume: info.VolumeSerialNumber,
        file_id: info.FileId.Identifier,
        change_time: basic.ChangeTime,
        write_time: basic.LastWriteTime,
    })
}

#[cfg(not(any(unix, windows)))]
fn platform_file_identity(
    _file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Option<PlatformFileIdentity> {
    Some(PlatformFileIdentity {})
}

#[derive(Debug)]
struct PreviewJob {
    window_seq: u64,
    pane_id: u64,
    generation: u64,
    request: VideoPasteRequest,
    /// The receipt's deadline, from its request.
    deadline: std::time::Instant,
}

struct PreviewWorkerLifetime(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for PreviewWorkerLifetime {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) struct VideoPreviewer {
    jobs: crossbeam_channel::Sender<PreviewJob>,
    live_workers: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl VideoPreviewer {
    /// Start the receipt threads. `media` makes posters on macOS and Linux;
    /// without it a checked video gets a receipt without one.
    pub(crate) fn new(
        proxy: winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
        media: Option<std::sync::Arc<kettle_media::client::WorkerClient>>,
    ) -> Self {
        let (jobs, receiver) = crossbeam_channel::bounded::<PreviewJob>(PREVIEW_QUEUE_CAPACITY);
        let live_workers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for index in 0..PREVIEW_THREAD_COUNT {
            let receiver = receiver.clone();
            let proxy = proxy.clone();
            let media = media.clone();
            let worker_lifetime = PreviewWorkerLifetime(live_workers.clone());
            // Increment before `spawn`: the new thread can exit immediately.
            // The captured guard balances this on normal exit, panic, or a
            // spawn error (which drops the unstarted closure).
            live_workers.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match std::thread::Builder::new()
                .name(format!("kettle-video-preview-{index}"))
                .spawn(move || {
                    let _worker_lifetime = worker_lifetime;
                    if !block_sigpipe_on_current_thread() {
                        log::warn!("video preview worker could not block SIGPIPE; worker stopped");
                        return;
                    }
                    while let Ok(job) = receiver.recv() {
                        let outcome = run_preview_child(job.request.path(), job.deadline);
                        let (candidate, preview) = match outcome {
                            PreviewChildOutcome::Ready(checked) => {
                                let size = checked.size;
                                let poster = poster(
                                    media.as_deref(),
                                    job.request.path(),
                                    checked,
                                    job.deadline,
                                );
                                let candidate =
                                    Some(VideoPasteCandidate::from_verified(job.request, size));
                                match poster {
                                    Ok(preview) => (candidate, preview),
                                    Err(ChildLost) => {
                                        // The file was checked: its receipt
                                        // stands, without the poster the
                                        // second check could not confirm.
                                        let _ = proxy.send_event(
                                            crate::app::UserEvent::VideoPreviewReady {
                                                window_seq: job.window_seq,
                                                pane_id: job.pane_id,
                                                generation: job.generation,
                                                candidate,
                                                preview: None,
                                            },
                                        );
                                        log::warn!(
                                            "video preview worker could not reap its child; worker stopped"
                                        );
                                        break;
                                    }
                                }
                            }
                            PreviewChildOutcome::Failed => (None, None),
                            PreviewChildOutcome::Skewed => {
                                note_worker_skew();
                                (None, None)
                            }
                            PreviewChildOutcome::WorkerLost => {
                                let _ =
                                    proxy.send_event(crate::app::UserEvent::VideoPreviewReady {
                                        window_seq: job.window_seq,
                                        pane_id: job.pane_id,
                                        generation: job.generation,
                                        candidate: None,
                                        preview: None,
                                    });
                                log::warn!(
                                    "video preview worker could not reap its child; worker stopped"
                                );
                                break;
                            }
                        };
                        let _ = proxy.send_event(crate::app::UserEvent::VideoPreviewReady {
                            window_seq: job.window_seq,
                            pane_id: job.pane_id,
                            generation: job.generation,
                            candidate,
                            preview,
                        });
                    }
                }) {
                Ok(_) => {}
                Err(error) => {
                    log::warn!("video preview worker could not start: {error}");
                }
            }
        }
        Self { jobs, live_workers }
    }

    pub(crate) fn request(
        &self,
        window_seq: u64,
        pane_id: u64,
        generation: u64,
        request: VideoPasteRequest,
    ) -> bool {
        if self.live_workers.load(std::sync::atomic::Ordering::Acquire) == 0 {
            return false;
        }
        self.jobs
            .try_send(PreviewJob {
                window_seq,
                pane_id,
                generation,
                request,
                deadline: std::time::Instant::now() + RECEIPT_DEADLINE,
            })
            .is_ok()
    }
}

#[cfg(unix)]
fn block_sigpipe_on_current_thread() -> bool {
    let mut signals = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: `signals` is initialized by sigemptyset before it is read, then
    // passed only to pthread_sigmask for this worker thread.
    unsafe {
        if libc::sigemptyset(signals.as_mut_ptr()) != 0 {
            return false;
        }
        let mut signals = signals.assume_init();
        if libc::sigaddset(&mut signals, libc::SIGPIPE) != 0 {
            return false;
        }
        libc::pthread_sigmask(
            libc::SIG_BLOCK,
            &signals,
            std::ptr::null_mut::<libc::sigset_t>(),
        ) == 0
    }
}

#[cfg(not(unix))]
fn block_sigpipe_on_current_thread() -> bool {
    true
}

#[derive(Debug, Eq, PartialEq)]
enum PreviewChildAttempt<T> {
    Ready(T),
    TimedOut,
    Failed,
    Skewed,
    WorkerLost,
}

#[derive(Debug, Eq, PartialEq)]
enum PreviewChildOutcome<T> {
    Ready(T),
    Failed,
    /// The worker is another Kettle build; previews wait for a restart.
    Skewed,
    WorkerLost,
}

/// Up to two attempts of a check, each given until when it may wait: its
/// own timeout, never past `deadline`. Only a timeout is retried, and only
/// while time is left; a job past its deadline starts no process.
fn retry_preview_timeout<T>(
    deadline: std::time::Instant,
    mut attempt: impl FnMut(std::time::Instant) -> PreviewChildAttempt<T>,
) -> PreviewChildOutcome<T> {
    for _ in 0..MAX_WORKER_ATTEMPTS {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        match attempt((now + WORKER_TIMEOUT).min(deadline)) {
            PreviewChildAttempt::Ready(output) => return PreviewChildOutcome::Ready(output),
            PreviewChildAttempt::TimedOut => {}
            PreviewChildAttempt::Failed => return PreviewChildOutcome::Failed,
            PreviewChildAttempt::Skewed => return PreviewChildOutcome::Skewed,
            PreviewChildAttempt::WorkerLost => return PreviewChildOutcome::WorkerLost,
        }
    }
    PreviewChildOutcome::Failed
}

fn worker_exit_is_retryable(code: Option<i32>) -> bool {
    code == Some(WORKER_TIMEOUT_EXIT)
}

/// Say once that the preview worker is another build: an update replaced
/// Kettle's executable, and previews wait for a restart.
fn note_worker_skew() {
    static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!(
            "video previews are off until Kettle restarts: an update replaced its \
             executable, and the preview helper is now another build"
        );
    }
}

/// The program to run as the preview worker: this very executable. On Linux
/// `/proc/self/exe` names the running image even after an update renamed or
/// deleted its file, so the worker is always this build. Elsewhere it is the
/// path Kettle started from, where an update may have put another build,
/// which then answers with [`WORKER_SKEW_EXIT`].
fn worker_executable() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(stand_in) = tests::STAND_IN.with(|stand_in| stand_in.borrow().clone()) {
        return Some(stand_in);
    }
    #[cfg(target_os = "linux")]
    {
        let image = Path::new("/proc/self/exe");
        if image.exists() {
            return Some(image.to_path_buf());
        }
    }
    std::env::current_exe().ok()
}

/// On macOS a write to a pipe with no reader raises SIGPIPE on the whole
/// process, not on the writing thread, so the queue worker's blocked SIGPIPE
/// does not cover it, and `main` restores the signal's default action, which
/// ends Kettle. Mark the pipe so the write fails with `EPIPE` instead.
#[cfg(target_os = "macos")]
fn no_sigpipe(stdin: &std::process::ChildStdin) -> bool {
    use std::os::fd::AsRawFd as _;
    /// From `<sys/fcntl.h>`; the `libc` crate lacks it for Apple targets.
    const F_SETNOSIGPIPE: libc::c_int = 73;
    // SAFETY: plain integers on a descriptor `stdin` owns and keeps open.
    unsafe { libc::fcntl(stdin.as_raw_fd(), F_SETNOSIGPIPE, 1) != -1 }
}

fn read_bounded_preview(reader: &mut impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader
        .take(MAX_OUTPUT_BYTES as u64 + 1)
        .read_to_end(&mut output)?;
    Ok(output)
}

fn stop_and_reap_child(child: &mut std::process::Child) -> bool {
    match child.kill() {
        Ok(()) => child.wait().is_ok(),
        Err(_) => matches!(child.try_wait(), Ok(Some(_))),
    }
}

/// Check `path`, by `deadline`.
fn run_preview_child(path: &Path, deadline: std::time::Instant) -> PreviewChildOutcome<Checked> {
    retry_preview_timeout(deadline, |until| run_preview_child_once(path, until))
}

/// One check of `path`, waited for until `until`: the request is written
/// and the reply read on their own threads, so a helper that stalls, before
/// or after reading, holds this one no longer than `until`, when it is
/// stopped, which ends both.
fn run_preview_child_once(path: &Path, until: std::time::Instant) -> PreviewChildAttempt<Checked> {
    let Some(input) = encode_request(path, build_identity()) else {
        return PreviewChildAttempt::Failed;
    };
    let Some(executable) = worker_executable() else {
        return PreviewChildAttempt::Failed;
    };
    let mut command = Command::new(executable);
    command
        .arg("__media-preview-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let Ok(mut child) = command.spawn() else {
        return PreviewChildAttempt::Failed;
    };
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return if stop_and_reap_child(&mut child) {
            PreviewChildAttempt::Failed
        } else {
            PreviewChildAttempt::WorkerLost
        };
    };
    #[cfg(target_os = "macos")]
    if !no_sigpipe(&stdin) {
        return if stop_and_reap_child(&mut child) {
            PreviewChildAttempt::Failed
        } else {
            PreviewChildAttempt::WorkerLost
        };
    }
    let writer = match std::thread::Builder::new()
        .name("kettle-video-preview-writer".to_owned())
        .spawn(move || stdin.write_all(&input))
    {
        Ok(writer) => writer,
        Err(error) => {
            log::warn!("video preview writer could not start: {error}");
            return if stop_and_reap_child(&mut child) {
                PreviewChildAttempt::Failed
            } else {
                PreviewChildAttempt::WorkerLost
            };
        }
    };
    let reader = match std::thread::Builder::new()
        .name("kettle-video-preview-reader".to_owned())
        .spawn(move || read_bounded_preview(&mut stdout))
    {
        Ok(reader) => reader,
        Err(error) => {
            log::warn!("video preview reader could not start: {error}");
            if !stop_and_reap_child(&mut child) {
                return PreviewChildAttempt::WorkerLost;
            }
            let _ = writer.join();
            return PreviewChildAttempt::Failed;
        }
    };
    let deadline = until;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                if !stop_and_reap_child(&mut child) {
                    return PreviewChildAttempt::WorkerLost;
                }
                let _ = writer.join();
                return match reader.join() {
                    Ok(Ok(_)) => PreviewChildAttempt::TimedOut,
                    Ok(Err(_)) | Err(_) => PreviewChildAttempt::Failed,
                };
            }
            Err(_) => {
                if !stop_and_reap_child(&mut child) {
                    return PreviewChildAttempt::WorkerLost;
                }
                // Join only to bound the threads. Whatever they return, the
                // failed wait already makes this a non-retryable failure.
                let _ = writer.join();
                let _ = reader.join();
                return PreviewChildAttempt::Failed;
            }
        }
    };
    let wrote = writer.join().is_ok_and(|wrote| wrote.is_ok());
    let output = reader.join().ok().and_then(Result::ok);
    finish_attempt(
        status.success(),
        status.code(),
        wrote,
        output,
        until,
        std::time::Instant::now(),
    )
}

/// What a check that ran to its helper's exit gave: its exit, whether its
/// whole request was written, and its reply, seen at `now`. A reply seen
/// after `until` is late and counts as a timeout, whatever it says; a
/// request the helper did not read, or a reply that could not be read, is
/// a failure.
fn finish_attempt(
    success: bool,
    code: Option<i32>,
    wrote: bool,
    output: Option<Vec<u8>>,
    until: std::time::Instant,
    now: std::time::Instant,
) -> PreviewChildAttempt<Checked> {
    if now > until {
        return PreviewChildAttempt::TimedOut;
    }
    if !wrote {
        return PreviewChildAttempt::Failed;
    }
    let Some(output) = output else {
        return PreviewChildAttempt::Failed;
    };
    if !success {
        return if worker_exit_is_retryable(code) {
            PreviewChildAttempt::TimedOut
        } else if code == Some(WORKER_SKEW_EXIT) {
            PreviewChildAttempt::Skewed
        } else {
            PreviewChildAttempt::Failed
        };
    }
    match decode_checked(&output) {
        Some(checked) => PreviewChildAttempt::Ready(checked),
        None => PreviewChildAttempt::Failed,
    }
}

/// A path as the frames carry it: its bytes on Unix, its UTF-16 code units
/// little-endian on Windows.
fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    }
}

/// The path `bytes` carry, as [`path_bytes`] wrote it.
fn path_from_bytes(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Some(PathBuf::from(OsString::from_vec(bytes.to_vec())))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        if bytes.len() & 1 != 0 {
            return None;
        }
        let wide = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();
        Some(PathBuf::from(OsString::from_wide(&wide)))
    }
}

fn encode_request(path: &Path, build: &str) -> Option<Vec<u8>> {
    let bytes = path_bytes(path);
    if bytes.is_empty() || bytes.len() > MAX_PATH_BYTES || build.len() > MAX_BUILD_IDENTITY_BYTES {
        return None;
    }
    let mut out = Vec::with_capacity(INPUT_MAGIC.len() + 1 + build.len() + 4 + bytes.len());
    out.extend_from_slice(INPUT_MAGIC);
    out.push(build.len() as u8);
    out.extend_from_slice(build.as_bytes());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytes);
    Some(out)
}

/// The path a request frame names, if `build` sent it. A frame from another
/// build (another frame version, or another identity) is [`Skewed`], so the
/// worker can say so instead of guessing at its layout.
///
/// [`Skewed`]: RequestError::Skewed
fn decode_request(input: &[u8], build: &str) -> Result<PathBuf, RequestError> {
    if input.len() < INPUT_MAGIC.len() + 1 || !input.starts_with(INPUT_MAGIC_FAMILY) {
        return Err(RequestError::Malformed);
    }
    if &input[..INPUT_MAGIC.len()] != INPUT_MAGIC {
        return Err(RequestError::Skewed);
    }
    let identity_len = usize::from(input[INPUT_MAGIC.len()]);
    let identity_start = INPUT_MAGIC.len() + 1;
    let path_start = identity_start + identity_len + 4;
    if identity_len > MAX_BUILD_IDENTITY_BYTES || input.len() < path_start {
        return Err(RequestError::Malformed);
    }
    if &input[identity_start..identity_start + identity_len] != build.as_bytes() {
        return Err(RequestError::Skewed);
    }
    let len = u32::from_le_bytes(
        input[path_start - 4..path_start]
            .try_into()
            .map_err(|_| RequestError::Malformed)?,
    ) as usize;
    if len == 0 || len > MAX_PATH_BYTES || input.len() != path_start + len {
        return Err(RequestError::Malformed);
    }
    path_from_bytes(&input[path_start..]).ok_or(RequestError::Malformed)
}

/// What the receipt check found.
#[derive(Debug, PartialEq, Eq)]
struct Checked {
    /// The video's size in bytes.
    size: u64,
    /// The opened video's device, inode, size and modification time, which
    /// the media worker is held to; zero on Windows, where none runs.
    identity: kettle_media::PathIdentity,
    /// The SHA-256 of the video's length and its first, middle and last
    /// 64 KiB, which a second check must find again.
    fingerprint: [u8; 32],
    /// The video's cached thumbnails, each opened as the video was (Linux).
    cached: Vec<CachedThumbnail>,
    /// The poster the platform made (Windows).
    preview: Option<RawPreview>,
}

/// A cached thumbnail the check opened: where, and the file it found there.
#[derive(Debug, PartialEq, Eq)]
struct CachedThumbnail {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

/// The check's reply: the magic, the size, the identity (device, inode,
/// seconds, nanoseconds), the fingerprint, the cached thumbnails (a count,
/// then each one's device, inode and path), then the poster's width, height
/// and RGBA, all zero for none.
fn encode_checked(checked: &Checked) -> Option<Vec<u8>> {
    let (width, height, rgba) = match &checked.preview {
        Some(preview) if valid_preview(preview.width, preview.height, preview.rgba.len()) => {
            (preview.width, preview.height, preview.rgba.as_slice())
        }
        Some(_) => return None,
        None => (0, 0, &[][..]),
    };
    if checked.cached.len() > MAX_CACHED_THUMBNAILS
        || checked.identity.size != checked.size
        || checked.identity.mtime_nanos >= 1_000_000_000
    {
        return None;
    }
    let identity = checked.identity;
    let mut out = Vec::with_capacity(64 + rgba.len());
    out.extend_from_slice(OUTPUT_MAGIC);
    out.extend_from_slice(&checked.size.to_le_bytes());
    out.extend_from_slice(&identity.dev.to_le_bytes());
    out.extend_from_slice(&identity.ino.to_le_bytes());
    out.extend_from_slice(&identity.mtime_seconds.to_le_bytes());
    out.extend_from_slice(&identity.mtime_nanos.to_le_bytes());
    out.extend_from_slice(&checked.fingerprint);
    out.push(checked.cached.len() as u8);
    for cached in &checked.cached {
        let path = path_bytes(&cached.path);
        if path.is_empty() || path.len() > MAX_PATH_BYTES {
            return None;
        }
        out.extend_from_slice(&cached.dev.to_le_bytes());
        out.extend_from_slice(&cached.ino.to_le_bytes());
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(&path);
    }
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
    out.extend_from_slice(rgba);
    Some(out)
}

/// A frame's fields, read in order.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.0.len() < len {
            return None;
        }
        let (head, tail) = self.0.split_at(len);
        self.0 = tail;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.array().map(u64::from_le_bytes)
    }
}

/// The check's reply, exactly as [`encode_checked`] writes it, or `None`.
fn decode_checked(output: &[u8]) -> Option<Checked> {
    let mut fields = Fields(output);
    if fields.take(OUTPUT_MAGIC.len())? != OUTPUT_MAGIC {
        return None;
    }
    let size = fields.u64()?;
    let identity = kettle_media::PathIdentity {
        dev: fields.u64()?,
        ino: fields.u64()?,
        size,
        mtime_seconds: i64::from_le_bytes(fields.array()?),
        mtime_nanos: fields.u32()?,
    };
    if identity.mtime_nanos >= 1_000_000_000 {
        return None;
    }
    let fingerprint = fields.array()?;
    let count = usize::from(fields.take(1)?[0]);
    if count > MAX_CACHED_THUMBNAILS {
        return None;
    }
    let mut cached = Vec::with_capacity(count);
    for _ in 0..count {
        let dev = fields.u64()?;
        let ino = fields.u64()?;
        let len = fields.u32()? as usize;
        if len == 0 || len > MAX_PATH_BYTES {
            return None;
        }
        let path = path_from_bytes(fields.take(len)?)?;
        cached.push(CachedThumbnail { path, dev, ino });
    }
    let width = fields.u32()?;
    let height = fields.u32()?;
    let len = fields.u32()? as usize;
    let rgba = fields.take(len)?;
    if !fields.0.is_empty() {
        return None;
    }
    let preview = if (width, height, len) == (0, 0, 0) {
        None
    } else if valid_preview(width, height, len) {
        Some(RawPreview {
            width,
            height,
            rgba: rgba.to_vec(),
        })
    } else {
        return None;
    };
    Some(Checked {
        size,
        identity,
        fingerprint,
        cached,
        preview,
    })
}

fn valid_preview(width: u32, height: u32, len: usize) -> bool {
    width > 0
        && height > 0
        && width <= MAX_PREVIEW_WIDTH
        && height <= MAX_PREVIEW_HEIGHT
        && usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            == Some(len)
        && len <= MAX_PREVIEW_BYTES
}

#[derive(Debug, PartialEq, Eq)]
struct RawPreview {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn sniff_held_video(file: &std::fs::File, len: u64) -> Option<kettle_media::video::VideoContainer> {
    let mut file = file.try_clone().ok()?;
    file.seek(std::io::SeekFrom::Start(0)).ok()?;
    let size = len.min(kettle_media::video::MAX_VIDEO_PREFIX_BYTES as u64) as usize;
    let mut prefix = vec![0; size];
    file.read_exact(&mut prefix).ok()?;
    kettle_media::video::sniff_video_container(&prefix, len)
}

pub fn run_worker() -> i32 {
    // The parent has its own receive deadline, but it may disappear while a
    // platform thumbnail provider is wedged. Bound the hidden child too so it
    // cannot outlive Kettle indefinitely.
    if std::thread::Builder::new()
        .name("kettle-video-preview-deadline".to_owned())
        .spawn(|| {
            std::thread::sleep(WORKER_TIMEOUT);
            std::process::exit(WORKER_TIMEOUT_EXIT);
        })
        .is_err()
    {
        return WORKER_WATCHDOG_SETUP_EXIT;
    }
    let mut input = Vec::new();
    if std::io::stdin()
        .take((INPUT_MAGIC.len() + 1 + MAX_BUILD_IDENTITY_BYTES + 4 + MAX_PATH_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .is_err()
    {
        return 2;
    }
    let path = match decode_request(&input, build_identity()) {
        Ok(path) => path,
        Err(RequestError::Skewed) => return WORKER_SKEW_EXIT,
        Err(RequestError::Malformed) => return 2,
    };
    let Some((identity, retained)) = file_identity(&path) else {
        return 3;
    };
    if sniff_held_video(&retained, identity.len).is_none() {
        return 3;
    }
    let Some(stamp) = stamp(&retained, identity.len) else {
        return 3;
    };
    // Keep the user-visible absolute path for the platform APIs. In particular,
    // `SHCreateItemFromParsingName` consumes a Shell parsing name, not the
    // extended-length canonical path used for identity. The identity checks
    // before and after extraction still reject path swaps. Source: Microsoft
    // `SHCreateItemFromParsingName` API documentation.
    #[cfg(windows)]
    let preview = platform_thumbnail(&path);
    #[cfg(not(windows))]
    let preview = None;
    #[cfg(target_os = "linux")]
    let cached = cached_thumbnails(&path);
    #[cfg(not(target_os = "linux"))]
    let cached = Vec::new();
    if !file_identity_matches(&path, &identity, &retained) {
        return 5;
    }
    let Some(output) = encode_checked(&Checked {
        size: identity.len,
        identity: stamp,
        fingerprint: identity.fingerprint,
        cached,
        preview,
    }) else {
        return 6;
    };
    if std::io::stdout().write_all(&output).is_err() {
        return 7;
    }
    0
}

#[cfg(windows)]
fn platform_thumbnail(path: &Path) -> Option<RawPreview> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
        GetDIBits, GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
    };
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, IBindCtx,
    };
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_RESIZETOFIT,
        SIIGBF_THUMBNAILONLY,
    };
    use windows::core::PCWSTR;

    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    struct BitmapGuard(HBITMAP);
    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.0.0));
            }
        }
    }

    // Thumbnail providers are Shell extension handlers. Microsoft
    // `Thumbnail Provider Guidelines` requires an apartment-threaded COM
    // model, so the isolated worker uses an STA.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .ok()?;
    let _guard = ComGuard;
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let item: IShellItemImageFactory =
        match unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>) } {
            Ok(item) => item,
            Err(error) => {
                log::debug!("Windows video preview could not create a Shell item: {error}");
                return None;
            }
        };
    let bitmap = match unsafe {
        item.GetImage(
            SIZE {
                cx: MAX_PREVIEW_WIDTH as i32,
                cy: MAX_PREVIEW_HEIGHT as i32,
            },
            SIIGBF_THUMBNAILONLY | SIIGBF_RESIZETOFIT,
        )
    } {
        Ok(bitmap) => BitmapGuard(bitmap),
        Err(error) => {
            log::debug!("Windows Shell did not return a video thumbnail: {error}");
            return None;
        }
    };
    let mut bitmap_shape = BITMAP::default();
    if unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(std::ptr::addr_of_mut!(bitmap_shape).cast()),
        )
    } == 0
    {
        return None;
    }
    let width = bitmap_shape.bmWidth.unsigned_abs();
    let height = bitmap_shape.bmHeight.unsigned_abs();
    if width == 0 || height == 0 || width > MAX_PREVIEW_WIDTH || height > MAX_PREVIEW_HEIGHT {
        return None;
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    let dc = unsafe { GetDC(None) };
    let rows = unsafe {
        GetDIBits(
            dc,
            bitmap.0,
            0,
            height,
            Some(bgra.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    unsafe {
        ReleaseDC(None, dc);
    }
    if rows != height as i32 {
        return None;
    }
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    // BI_RGB does not define the high byte as alpha. Some Shell providers
    // return zero there, which would make an otherwise valid poster invisible
    // in the renderer's straight-alpha pipeline.
    force_opaque_alpha(&mut bgra);
    Some(RawPreview {
        width,
        height,
        rgba: bgra,
    })
}

/// The opened video's identity, as the media worker will read it from its
/// own descriptor; on Windows, where no worker runs, only its size.
fn stamp(file: &std::fs::File, size: u64) -> Option<kettle_media::PathIdentity> {
    #[cfg(unix)]
    {
        let identity = kettle_media::PathIdentity::of(&file.metadata().ok()?)?;
        (identity.size == size).then_some(identity)
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Some(kettle_media::PathIdentity {
            dev: 0,
            ino: 0,
            size,
            mtime_seconds: 0,
            mtime_nanos: 0,
        })
    }
}

/// What every receipt job is drawn against: a video's frame or a cached
/// thumbnail ignores the theme, and the receipt paints its own canvas.
#[cfg(unix)]
const RECEIPT_THEME: kettle_media::Theme = kettle_media::Theme {
    background: [0, 0, 0, 255],
    foreground: [255; 4],
    palette: [[0, 0, 0, 255]; 16],
    accent: [255; 4],
    is_dark: true,
};

/// A check's helper that could not be reaped: the queue thread that ran it
/// stops rather than start another beside it.
#[derive(Debug, PartialEq, Eq)]
struct ChildLost;

/// The receipt's poster: on Windows the one the check brought back, on macOS
/// and Linux one the media worker makes, or none.
fn poster(
    media: Option<&kettle_media::client::WorkerClient>,
    path: &Path,
    checked: Checked,
    deadline: std::time::Instant,
) -> Result<Option<kettle_core::ImageData>, ChildLost> {
    #[cfg(unix)]
    {
        let Some(media) = media else {
            return Ok(None);
        };
        worker_poster(
            path,
            &checked,
            deadline,
            |job, control| media.render_media_with_control(job, control),
            || run_preview_child(path, deadline),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (media, path, deadline);
        Ok(checked.preview.and_then(|preview| {
            kettle_core::ImageData::new(preview.width, preview.height, preview.rgba)
        }))
    }
}

/// A poster from the media worker, rendered through `render` by `deadline`:
/// the video's own, as an Auto job held to the inode the check opened, kept
/// only when it is a video's and its file is unchanged since the check. On
/// Linux, when no decoder could make one, the first cached thumbnail the
/// worker shows is of this video, as the check saw it. Either way it is
/// kept only when a second check, `recheck`, finds the same file with the
/// same contents, still held as the first check held it: the worker read it
/// on its own, and a video rewritten in between, its time restored, or made
/// writable by another user, gets no poster.
#[cfg(unix)]
fn worker_poster(
    path: &Path,
    checked: &Checked,
    deadline: std::time::Instant,
    render: impl FnMut(
        &kettle_media::Job,
        &kettle_media::client::RenderControl,
    ) -> Result<kettle_media::RenderOutput, kettle_media::client::RenderError>,
    recheck: impl FnOnce() -> PreviewChildOutcome<Checked>,
) -> Result<Option<kettle_core::ImageData>, ChildLost> {
    let Some(image) = rendered_poster(path, checked, deadline, render) else {
        return Ok(None);
    };
    match recheck() {
        PreviewChildOutcome::Ready(again)
            if again.size == checked.size
                && again.identity == checked.identity
                && again.fingerprint == checked.fingerprint =>
        {
            Ok(Some(image))
        }
        PreviewChildOutcome::WorkerLost => Err(ChildLost),
        _ => {
            log::debug!("video paste receipt: no poster: the video changed after its check");
            Ok(None)
        }
    }
}

/// The poster [`worker_poster`] renders, before its second check.
#[cfg(unix)]
fn rendered_poster(
    path: &Path,
    checked: &Checked,
    deadline: std::time::Instant,
    mut render: impl FnMut(
        &kettle_media::Job,
        &kettle_media::client::RenderControl,
    ) -> Result<kettle_media::RenderOutput, kettle_media::client::RenderError>,
) -> Option<kettle_core::ImageData> {
    use kettle_media::client::{RenderControl, RenderError};
    use kettle_media::{JobKind, MediaKind};

    let control = RenderControl::with_deadline(deadline);
    let video = receipt_job(
        JobKind::Auto,
        attested(path, checked.identity.dev, checked.identity.ino)?,
    );
    let failure = match render(&video, &control) {
        Ok(output) => {
            let unchanged = output.rendered.digest.path_identity == Some(checked.identity);
            if output.kind != MediaKind::Video || !unchanged {
                log::debug!(
                    "video paste receipt: no poster from a {:?} render, unchanged {unchanged}",
                    output.kind
                );
                return None;
            }
            return receipt_image(output.rendered);
        }
        Err(RenderError::Failure(failure)) => failure,
        Err(RenderError::Cancelled) => return None,
    };
    log::debug!("video paste receipt: no poster from the worker: {failure:?}");
    #[cfg(target_os = "linux")]
    if no_decoder_made_one(failure) {
        return cached_poster(path, checked, &control, render);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = failure;
    None
}

/// Whether a video's poster failed for want of a decoder that could make
/// one, which a cached thumbnail may stand in for: none, one without the
/// codec or container, one the worker could not confine, or one that failed
/// on the stream. A video that changed, a deadline that passed or a worker
/// that is missing is not.
#[cfg(target_os = "linux")]
fn no_decoder_made_one(failure: kettle_media::FailureCode) -> bool {
    use kettle_media::FailureCode;
    matches!(
        failure,
        FailureCode::BackendUnavailable
            | FailureCode::CodecUnavailable
            | FailureCode::UnsupportedContainer
            | FailureCode::SandboxUnavailable
            | FailureCode::RenderParse
            | FailureCode::RenderResource
    )
}

/// The first of the check's cached thumbnails the worker renders as this
/// video's: named by its URI and stamped with the modification time the
/// check saw. Each is held to the inode the check opened.
#[cfg(target_os = "linux")]
fn cached_poster(
    path: &Path,
    checked: &Checked,
    control: &kettle_media::client::RenderControl,
    mut render: impl FnMut(
        &kettle_media::Job,
        &kettle_media::client::RenderControl,
    ) -> Result<kettle_media::RenderOutput, kettle_media::client::RenderError>,
) -> Option<kettle_core::ImageData> {
    use kettle_media::client::RenderError;
    use kettle_media::{FailureCode, JobKind, MediaKind, ThumbnailOf};

    let of = ThumbnailOf {
        uri_sha256: ThumbnailOf::uri_digest(&thumbnail_uri(path)?),
        mtime_seconds: checked.identity.mtime_seconds,
        mtime_nanos: checked.identity.mtime_nanos,
    };
    for cached in &checked.cached {
        let job = receipt_job(
            JobKind::CachedThumbnail(of),
            attested(&cached.path, cached.dev, cached.ino)?,
        );
        match render(&job, control) {
            Ok(output) if output.kind == MediaKind::Raster => {
                return receipt_image(output.rendered);
            }
            Ok(_)
            | Err(RenderError::Cancelled | RenderError::Failure(FailureCode::RenderTimeout)) => {
                return None;
            }
            Err(RenderError::Failure(_)) => {}
        }
    }
    None
}

/// A source the worker opens only when it is the file `dev` and `ino` name.
#[cfg(unix)]
fn attested(path: &Path, dev: u64, ino: u64) -> Option<kettle_media::Source> {
    Some(kettle_media::Source::Path {
        path: kettle_media::NativePath::from_path(path).ok()?,
        authorization: kettle_media::Authorization::ExternalAttested(
            kettle_media::ExternalAttested { dev, ino },
        ),
    })
}

/// A receipt's job: `kind` over `source`, fitted inside the receipt's
/// largest poster.
#[cfg(unix)]
fn receipt_job(kind: kettle_media::JobKind, source: kettle_media::Source) -> kettle_media::Job {
    kettle_media::Job {
        kind,
        source,
        theme: RECEIPT_THEME,
        canvas: kettle_media::Canvas::Theme,
        target: kettle_media::Target {
            width: MAX_PREVIEW_WIDTH,
            height: MAX_PREVIEW_HEIGHT,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts: Vec::new(),
    }
}

/// A rendered poster as the receipt draws it: straight RGBA within the
/// receipt's bounds.
#[cfg(unix)]
fn receipt_image(rendered: kettle_media::Rendered) -> Option<kettle_core::ImageData> {
    if !valid_preview(rendered.width, rendered.height, rendered.rgba.len()) {
        return None;
    }
    kettle_core::ImageData::new(rendered.width, rendered.height, rendered.rgba)
}

#[cfg(any(target_os = "linux", test))]
fn thumbnail_digest_hex(digest: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(32);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// The video's file URI, as Freedesktop.org's `Thumbnail Managing Standard`
/// ("Thumbnail URI") spells it for the cache's file names and `Thumb::URI`.
#[cfg(target_os = "linux")]
fn thumbnail_uri(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

/// The video's cached thumbnails in the user's cache, largest size first.
#[cfg(target_os = "linux")]
fn cached_thumbnails(path: &Path) -> Vec<CachedThumbnail> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
    cache.map_or_else(Vec::new, |cache| cached_thumbnails_in(&cache, path))
}

/// The video's thumbnails under `cache`, largest size first: each named by
/// the MD5 of the video's URI, as the standard's "Thumbnail Creation" names
/// them, and opened through kettle-state's held, trusted parent chain, which
/// refuses a link, a file another user could change, or a cache directory
/// another user could swap. Nothing is parsed here; the worker reads each.
#[cfg(target_os = "linux")]
fn cached_thumbnails_in(cache: &Path, path: &Path) -> Vec<CachedThumbnail> {
    use md5::{Digest as _, Md5};
    use std::os::unix::fs::MetadataExt as _;

    let Some(uri) = thumbnail_uri(path) else {
        return Vec::new();
    };
    let digest = thumbnail_digest_hex(Md5::digest(uri.as_bytes()).into());
    ["xx-large", "x-large", "large", "normal"]
        .into_iter()
        .filter_map(|class| {
            let candidate = cache
                .join("thumbnails")
                .join(class)
                .join(format!("{digest}.png"));
            let metadata = kettle_state::open_trusted_file_read(&candidate)
                .ok()?
                .metadata()
                .ok()?;
            Some(CachedThumbnail {
                path: candidate,
                dev: metadata.dev(),
                ino: metadata.ino(),
            })
        })
        .collect()
}

#[cfg(windows)]
fn force_opaque_alpha(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// A program standing in for the check's helper, for the attempts
        /// this thread makes.
        pub(super) static STAND_IN: std::cell::RefCell<Option<PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A reply seen after the attempt's cutoff is late, whatever it says;
    /// an unwritten request or an unread reply fails; the exit codes keep
    /// their meanings.
    #[test]
    fn an_attempt_s_reply_counts_only_by_its_cutoff() {
        let frame = || Some(encode_checked(&bare_check()).unwrap());
        let until = std::time::Instant::now();
        let after = until + Duration::from_millis(1);
        assert_eq!(
            finish_attempt(true, Some(0), true, frame(), until, after),
            PreviewChildAttempt::TimedOut
        );
        assert_eq!(
            finish_attempt(true, Some(0), true, frame(), until, until),
            PreviewChildAttempt::Ready(bare_check())
        );
        assert_eq!(
            finish_attempt(true, Some(0), false, frame(), until, until),
            PreviewChildAttempt::Failed
        );
        assert_eq!(
            finish_attempt(true, Some(0), true, None, until, until),
            PreviewChildAttempt::Failed
        );
        assert_eq!(
            finish_attempt(
                false,
                Some(WORKER_TIMEOUT_EXIT),
                true,
                frame(),
                until,
                until
            ),
            PreviewChildAttempt::TimedOut
        );
        assert_eq!(
            finish_attempt(false, Some(WORKER_SKEW_EXIT), true, frame(), until, until),
            PreviewChildAttempt::Skewed
        );
        assert_eq!(
            finish_attempt(false, Some(3), true, frame(), until, until),
            PreviewChildAttempt::Failed
        );
    }

    /// A helper that stalls without reading a request larger than a pipe
    /// holds stops the attempt at its cutoff, not when the helper ends.
    #[cfg(unix)]
    #[test]
    fn a_stalled_helper_holds_an_attempt_no_longer_than_its_cutoff() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::test_tempdir();
        let helper = dir.path().join("stalls");
        std::fs::write(&helper, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = PathBuf::from(format!("/{}", "a".repeat(MAX_PATH_BYTES - 1)));
        let attempt = std::thread::spawn(move || {
            assert!(block_sigpipe_on_current_thread());
            STAND_IN.with(|stand_in| *stand_in.borrow_mut() = Some(helper));
            let started = std::time::Instant::now();
            let attempt = run_preview_child_once(&path, started + Duration::from_millis(300));
            (attempt, started.elapsed())
        });
        let (attempt, elapsed) = attempt.join().unwrap();
        assert!(
            matches!(
                attempt,
                PreviewChildAttempt::TimedOut | PreviewChildAttempt::Failed
            ),
            "{attempt:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the attempt waited {elapsed:?} for a stalled helper"
        );
    }

    /// A second check whose helper cannot be reaped stops the queue thread,
    /// which still answers the checked receipt, without a poster.
    #[cfg(unix)]
    #[test]
    fn an_unreapable_second_check_stops_its_queue_thread() {
        use kettle_media::MediaKind;
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let checked = bare_check();
        let mut rendered = false;
        let result = worker_poster(
            &path,
            &checked,
            std::time::Instant::now() + RECEIPT_DEADLINE,
            |_, _| {
                rendered = true;
                worker_reply(MediaKind::Video, Some(checked.identity))
            },
            || PreviewChildOutcome::WorkerLost,
        );
        assert!(rendered);
        assert_eq!(result.err(), Some(ChildLost));
        let source = kettle_test_support::production_source(include_str!("video_preview.rs"));
        let lost = source
            .split("Err(ChildLost) => {")
            .nth(1)
            .and_then(|rest| rest.split("\n                                    }").next())
            .expect("the second check's lost-child arm");
        assert!(
            lost.contains("candidate,")
                && lost.contains("preview: None")
                && lost.contains("break;"),
            "{lost}"
        );
    }

    #[cfg(unix)]
    const VIDEO_FIXTURE: &[u8] = include_bytes!("../testdata/video-preview.mp4");

    fn write_test_video(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn trusted_candidate(
        paths: &[PathBuf],
        source: VideoPasteSource,
    ) -> Option<(
        VideoPasteCandidate,
        FileIdentity,
        std::sync::Arc<std::fs::File>,
    )> {
        let request = VideoPasteRequest::from_user_paths(paths, source)?;
        let (identity, retained) = file_identity(request.path())?;
        let candidate = VideoPasteCandidate::from_verified(request, identity.len);
        Some((candidate, identity, retained))
    }

    #[test]
    fn video_candidate_requires_an_explicit_regular_local_video() {
        let dir = crate::test_tempdir();
        let video = dir.path().join("clip.MP4");
        write_test_video(&video, b"fixture");
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"fixture").unwrap();

        let (candidate, identity, retained) =
            trusted_candidate(&[text, video.clone()], VideoPasteSource::Clipboard)
                .expect("the explicit video path is eligible");
        assert_eq!(candidate.path, video);
        assert_eq!(candidate.count, 1);
        assert_eq!(candidate.extension, "MP4");
        assert_eq!(candidate.size, 7);
        assert!(file_identity_matches(&candidate.path, &identity, &retained));

        std::fs::write(&candidate.path, b"changed").unwrap();
        assert!(
            !file_identity_matches(&candidate.path, &identity, &retained),
            "a replaced source invalidates its poster"
        );
    }

    #[test]
    fn typescript_extensions_are_not_video_receipts() {
        let dir = crate::test_tempdir();
        assert_eq!(video_extension(&dir.path().join("app.ts")), None);
        assert_eq!(video_extension(&dir.path().join("module.mts")), None);
        assert_eq!(
            video_extension(&dir.path().join("stream.m2ts")).as_deref(),
            Some("M2TS")
        );
    }

    #[test]
    fn video_request_counts_later_video_paths_without_opening_them() {
        let dir = crate::test_tempdir();
        let first = dir.path().join("first.mp4");
        write_test_video(&first, b"fixture");
        let missing_video = dir.path().join("slow-share.mp4");
        let missing_text = dir.path().join("notes.txt");

        let request = VideoPasteRequest::from_user_paths(
            &[first, missing_video, missing_text],
            VideoPasteSource::Drop,
        )
        .expect("the first video-like path supplies the request");

        assert_eq!(request.count, 2, "later video-like paths are cosmetic");
    }

    #[test]
    fn video_request_construction_never_probes_the_file_system() {
        let dir = crate::test_tempdir();
        let missing = dir.path().join("missing.mp4");
        let later = dir.path().join("later.webm");

        assert!(
            VideoPasteRequest::from_user_paths(&[missing, later], VideoPasteSource::Drop).is_some(),
            "event-loop request construction must classify syntax without touching either path"
        );
    }

    #[test]
    fn a_previewer_without_live_workers_rejects_jobs() {
        let (jobs, _receiver) = crossbeam_channel::bounded(PREVIEW_QUEUE_CAPACITY);
        let live_workers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let previewer = VideoPreviewer {
            jobs,
            live_workers: live_workers.clone(),
        };
        drop(PreviewWorkerLifetime(live_workers));
        let request = VideoPasteRequest::from_user_paths(
            &[std::env::current_dir().unwrap().join("missing.mp4")],
            VideoPasteSource::Clipboard,
        )
        .unwrap();

        assert!(!previewer.request(1, 2, 3, request));
        assert!(
            PENDING_RECEIPT_TIMEOUT >= RECEIPT_DEADLINE + Duration::from_secs(2),
            "pending state must outlive the receipt's deadline and its cleanup"
        );
    }

    /// A job carries its deadline from its request, so time spent queued
    /// counts against it.
    #[test]
    fn a_receipt_job_carries_its_deadline_from_the_request() {
        let (jobs, receiver) = crossbeam_channel::bounded(PREVIEW_QUEUE_CAPACITY);
        let live_workers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let previewer = VideoPreviewer { jobs, live_workers };
        let request = VideoPasteRequest::from_user_paths(
            &[std::env::current_dir().unwrap().join("clip.mp4")],
            VideoPasteSource::Drop,
        )
        .unwrap();
        let before = std::time::Instant::now();
        assert!(previewer.request(1, 2, 3, request));
        let job = receiver.try_recv().unwrap();
        assert!(job.deadline >= before + RECEIPT_DEADLINE);
        assert!(job.deadline <= std::time::Instant::now() + RECEIPT_DEADLINE);
    }

    #[test]
    fn preview_child_retries_only_one_timeout() {
        let later = std::time::Instant::now() + Duration::from_secs(60);
        let mut calls = 0;
        let mut outcomes = [
            PreviewChildAttempt::TimedOut,
            PreviewChildAttempt::Ready(7_u8),
        ]
        .into_iter();
        let result = retry_preview_timeout(later, |_| {
            calls += 1;
            outcomes.next().unwrap()
        });
        assert_eq!(result, PreviewChildOutcome::Ready(7));
        assert_eq!(calls, 2, "one cold timeout gets one fresh worker");

        calls = 0;
        let result = retry_preview_timeout(later, |_| {
            calls += 1;
            PreviewChildAttempt::<u8>::Failed
        });
        assert_eq!(result, PreviewChildOutcome::Failed);
        assert_eq!(calls, 1, "non-timeout failures must stay fail-closed");

        calls = 0;
        let result = retry_preview_timeout(later, |_| {
            calls += 1;
            PreviewChildAttempt::<u8>::TimedOut
        });
        assert_eq!(result, PreviewChildOutcome::Failed);
        assert_eq!(calls, 2, "repeated timeouts must remain bounded");

        calls = 0;
        let result = retry_preview_timeout(later, |_| {
            calls += 1;
            PreviewChildAttempt::<u8>::WorkerLost
        });
        assert_eq!(result, PreviewChildOutcome::WorkerLost);
        assert_eq!(calls, 1, "an unreaped child poisons its queue worker");
    }

    /// Each attempt waits its own timeout and never past the receipt's
    /// deadline; a job past it starts none, and a timeout is retried only
    /// while time is left.
    #[test]
    fn check_attempts_end_by_the_receipt_s_deadline() {
        let now = std::time::Instant::now();
        let mut calls = 0;
        let result = retry_preview_timeout(now, |_| {
            calls += 1;
            PreviewChildAttempt::Ready(())
        });
        assert_eq!(result, PreviewChildOutcome::Failed);
        assert_eq!(calls, 0, "a job past its deadline starts no check");

        let soon = std::time::Instant::now() + Duration::from_millis(50);
        let mut waits = Vec::new();
        let result = retry_preview_timeout(soon, |until| {
            waits.push(until);
            std::thread::sleep(Duration::from_millis(60));
            PreviewChildAttempt::<()>::TimedOut
        });
        assert_eq!(result, PreviewChildOutcome::Failed);
        assert_eq!(waits, vec![soon], "no retry once the deadline passed");

        let later = std::time::Instant::now() + Duration::from_secs(60);
        let before = std::time::Instant::now();
        let mut waits = Vec::new();
        retry_preview_timeout(later, |until| {
            waits.push(until);
            PreviewChildAttempt::Ready(())
        });
        assert!(waits[0] >= before + WORKER_TIMEOUT);
        assert!(waits[0] <= std::time::Instant::now() + WORKER_TIMEOUT);
    }

    #[test]
    fn only_the_worker_deadline_exit_is_retryable() {
        assert!(worker_exit_is_retryable(Some(WORKER_TIMEOUT_EXIT)));
        assert!(!worker_exit_is_retryable(Some(WORKER_WATCHDOG_SETUP_EXIT)));
        assert!(!worker_exit_is_retryable(Some(1)));
        assert!(!worker_exit_is_retryable(None));
    }

    #[test]
    fn a_reader_error_discards_even_a_complete_preview_frame() {
        struct CompleteThenError {
            bytes: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for CompleteThenError {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let read = self.bytes.read(buffer)?;
                if read == 0 {
                    Err(std::io::Error::other("injected reader failure"))
                } else {
                    Ok(read)
                }
            }
        }

        let frame = encode_checked(&checked(Vec::new(), None)).unwrap();
        let mut reader = CompleteThenError {
            bytes: std::io::Cursor::new(frame.clone()),
        };
        assert!(
            read_bounded_preview(&mut reader).is_err(),
            "partial success must not authenticate bytes from a failed pipe read"
        );
        assert!(decode_checked(&frame).is_some(), "fixture must be valid");
    }

    #[test]
    fn preview_child_handles_reader_thread_spawn_failure() {
        let source = kettle_test_support::production_source(include_str!("video_preview.rs"));
        let body = source
            .split("fn run_preview_child_once(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn encode_request(").next())
            .expect("run_preview_child_once body");
        assert!(
            !body.contains("std::thread::spawn("),
            "an infallible reader spawn aborts release builds when thread creation fails"
        );
        let spawn_error = body
            .split_once("Err(error) =>")
            .and_then(|(_, rest)| rest.split_once("\n    let deadline"))
            .expect("reader spawn error arm")
            .0;

        assert!(
            body.contains("std::thread::Builder::new()")
                && body.contains(".name(\"kettle-video-preview-reader\".to_owned())")
                && spawn_error.contains("stop_and_reap_child(&mut child)")
                && spawn_error.contains("PreviewChildAttempt::WorkerLost"),
            "reader creation must be fallible and poison the queue worker when the child cannot be reaped"
        );
    }

    #[test]
    fn an_unreaped_child_stops_its_queue_worker() {
        let source = kettle_test_support::production_source(include_str!("video_preview.rs"));
        let lost_arm = source
            .split("PreviewChildOutcome::WorkerLost => {")
            .nth(1)
            .and_then(|rest| rest.split("\n                            }").next())
            .expect("worker-lost outcome arm");
        assert!(
            lost_arm.contains("candidate: None")
                && lost_arm.contains("preview: None")
                && lost_arm.contains("break;"),
            "an unreaped child must clear the current receipt and stop that queue worker"
        );
    }

    #[test]
    fn a_parent_timeout_reaps_before_accepting_reader_completion() {
        let source = kettle_test_support::production_source(include_str!("video_preview.rs"));
        let body = source
            .split("fn run_preview_child_once(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn encode_request(").next())
            .expect("run_preview_child_once body");
        let timeout_arm = body
            .split("Ok(None) => {")
            .nth(1)
            .and_then(|rest| rest.split("\n            Err(_) => {").next())
            .expect("parent timeout arm");
        let reap = timeout_arm
            .find("stop_and_reap_child(&mut child)")
            .expect("a timed-out child must be reaped");
        let join = timeout_arm
            .find("reader.join()")
            .expect("the pipe reader must be joined");

        assert!(
            reap < join,
            "reap the child before trusting pipe completion"
        );
        assert!(
            timeout_arm.contains("Ok(Ok(_)) => PreviewChildAttempt::TimedOut")
                && timeout_arm.contains("Ok(Err(_)) | Err(_) => PreviewChildAttempt::Failed"),
            "only a clean reader completion may make the timeout retryable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preview_workers_block_sigpipe_before_writing_to_children() {
        std::thread::spawn(|| {
            assert!(block_sigpipe_on_current_thread());

            let mut current = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
            // SAFETY: a null set queries the current thread mask into `current`.
            let queried = unsafe {
                libc::pthread_sigmask(
                    libc::SIG_BLOCK,
                    std::ptr::null::<libc::sigset_t>(),
                    current.as_mut_ptr(),
                )
            };
            assert_eq!(queried, 0);
            // SAFETY: pthread_sigmask initialized `current` on success.
            assert_eq!(
                unsafe { libc::sigismember(&current.assume_init(), libc::SIGPIPE) },
                1
            );
        })
        .join()
        .expect("SIGPIPE mask probe thread");
    }

    #[cfg(unix)]
    #[test]
    fn a_preview_child_that_exits_unread_cannot_end_kettle() {
        const CHILD: &str = "KETTLE_VIDEO_PREVIEW_SIGPIPE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "video_preview::tests::a_preview_child_that_exits_unread_cannot_end_kettle",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child failed: {}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // As Kettle's `main` leaves it.
        // SAFETY: this child process runs this one test; plain integers.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
        // On a queue worker's thread, as in production. The worker here is
        // this test binary, which runs no test for the worker argument and
        // exits without reading: a request larger than a pipe holds meets a
        // closed pipe.
        let attempt = std::thread::spawn(|| {
            assert!(block_sigpipe_on_current_thread());
            let path = PathBuf::from(format!("/{}", "a".repeat(MAX_PATH_BYTES - 1)));
            matches!(
                run_preview_child_once(&path, std::time::Instant::now() + WORKER_TIMEOUT),
                PreviewChildAttempt::Failed | PreviewChildAttempt::WorkerLost
            )
        })
        .join()
        .expect("preview attempt thread");
        assert!(attempt, "an unread request is a failed attempt");
    }

    #[cfg(unix)]
    #[test]
    fn video_candidate_rejects_symlinks_and_relative_paths() {
        use std::os::unix::fs::symlink;

        let dir = crate::test_tempdir();
        let video = dir.path().join("clip.webm");
        write_test_video(&video, b"fixture");
        let link = dir.path().join("linked.webm");
        symlink(&video, &link).unwrap();

        let request = VideoPasteRequest::from_user_paths(&[link], VideoPasteSource::Drop).unwrap();
        assert!(file_identity(request.path()).is_none());
        assert!(
            VideoPasteRequest::from_user_paths(
                &[PathBuf::from("relative.mp4")],
                VideoPasteSource::Clipboard,
            )
            .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn video_candidate_rejects_a_replacement_with_preserved_size_and_mtime() {
        let dir = crate::test_tempdir();
        let video = dir.path().join("clip.mp4");
        write_test_video(&video, b"original");
        let (candidate, identity, retained) =
            trusted_candidate(std::slice::from_ref(&video), VideoPasteSource::Clipboard).unwrap();

        let original_mtime = std::fs::metadata(&video).unwrap().modified().unwrap();
        let replacement = dir.path().join("replacement.mp4");
        write_test_video(&replacement, b"swapped!");
        let replacement_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .unwrap();
        replacement_file
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        std::fs::rename(&replacement, &video).unwrap();

        assert_eq!(std::fs::metadata(&video).unwrap().len(), candidate.size);
        assert_eq!(
            std::fs::metadata(&video).unwrap().modified().unwrap(),
            original_mtime
        );
        assert!(
            !file_identity_matches(&candidate.path, &identity, &retained),
            "replacing the source object must invalidate a same-size, same-mtime poster"
        );
    }

    #[cfg(unix)]
    #[test]
    fn video_candidate_rejects_an_untrusted_parent_directory() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = crate::test_tempdir();
        let writable = dir.path().join("shared");
        std::fs::create_dir(&writable).unwrap();
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o777)).unwrap();
        let video = writable.join("clip.mp4");
        write_test_video(&video, VIDEO_FIXTURE);

        let request =
            VideoPasteRequest::from_user_paths(&[video], VideoPasteSource::Clipboard).unwrap();
        let identity = file_identity(request.path());
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            identity.is_none(),
            "a path another principal can replace must not get a poster receipt"
        );
    }

    #[test]
    fn worker_protocol_rejects_trailing_and_oversized_payloads() {
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let encoded = encode_request(&path, "1.0 (abc)").unwrap();
        assert_eq!(decode_request(&encoded, "1.0 (abc)"), Ok(path));

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(
            decode_request(&trailing, "1.0 (abc)"),
            Err(RequestError::Malformed)
        );

        let mut wrong_magic = encoded;
        wrong_magic[0] ^= 0xff;
        assert_eq!(
            decode_request(&wrong_magic, "1.0 (abc)"),
            Err(RequestError::Malformed)
        );
        assert!(encode_request(&PathBuf::from("/a"), &"x".repeat(129)).is_none());
    }

    /// A request from another build, by its frame version or its identity,
    /// is skew: the worker says so with its own exit code, which the parent
    /// does not retry and reports once.
    #[test]
    fn another_build_s_request_is_skew() {
        let path = PathBuf::from("/tmp/clip.mp4");
        let request = encode_request(&path, "4.9.0 (aaaaaaaaaaaa)").unwrap();
        assert_eq!(
            decode_request(&request, "5.0.0 (bbbbbbbbbbbb)"),
            Err(RequestError::Skewed)
        );
        // The frame before build identities: same family, older version.
        let mut old = b"KTLVPIN1".to_vec();
        old.extend_from_slice(&13u32.to_le_bytes());
        old.extend_from_slice(b"/tmp/clip.mp4");
        assert_eq!(decode_request(&old, "4.9.0"), Err(RequestError::Skewed));

        let mut calls = 0;
        let later = std::time::Instant::now() + Duration::from_secs(60);
        let result = retry_preview_timeout(later, |_| {
            calls += 1;
            PreviewChildAttempt::<u8>::Skewed
        });
        assert_eq!(result, PreviewChildOutcome::Skewed);
        assert_eq!(calls, 1, "skew is not retried");
        assert!(!worker_exit_is_retryable(Some(WORKER_SKEW_EXIT)));

        let src = include_str!("video_preview.rs");
        let child = src
            .split("fn run_preview_child_once(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn encode_request(").next())
            .expect("run_preview_child_once");
        assert!(child.contains("code == Some(WORKER_SKEW_EXIT)"));
        assert!(child.contains("encode_request(path, build_identity())"));
        let worker = src
            .split("pub fn run_worker() -> i32 {")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("run_worker");
        assert!(worker.contains("Err(RequestError::Skewed) => return WORKER_SKEW_EXIT,"));
    }

    /// The identity is what `main` records, bounded.
    #[test]
    fn the_build_identity_is_recorded_once_and_bounded() {
        set_build_identity(&format!("9.9.9 (abc+{})", "f".repeat(200)));
        assert!(build_identity().starts_with("9.9.9 (abc+"));
        assert_eq!(build_identity().len(), MAX_BUILD_IDENTITY_BYTES);
        set_build_identity("another");
        assert!(build_identity().starts_with("9.9.9"), "set once");
    }

    /// On Linux the worker is the running image itself, so an update that
    /// replaced or deleted Kettle's file cannot hand the parent another
    /// build. Elsewhere it is the path Kettle started from.
    #[test]
    fn the_worker_is_this_executable() {
        let worker = worker_executable().expect("a worker program");
        if cfg!(target_os = "linux") {
            assert_eq!(worker, PathBuf::from("/proc/self/exe"));
        } else {
            assert_eq!(worker, std::env::current_exe().unwrap());
        }
    }

    /// A copy of this test binary deletes its own file, as an update that
    /// replaced Kettle would, then starts its worker program: on Linux that
    /// still runs, because it names the running image, not the path.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_worker_runs_after_its_file_is_deleted() {
        use std::os::unix::fs::PermissionsExt as _;
        const CHILD: &str = "KETTLE_TEST_DELETED_WORKER_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let me = std::env::current_exe().unwrap();
            std::fs::remove_file(&me).unwrap();
            assert!(!me.exists());
            let status = Command::new(worker_executable().unwrap())
                .args(["--list"])
                .stdout(Stdio::null())
                .status()
                .expect("start the worker program after the file is gone");
            assert!(status.success());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("kettle-ui-test-copy");
        std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755)).unwrap();
        let status = Command::new(&copy)
            .args([
                "--exact",
                "video_preview::tests::the_worker_runs_after_its_file_is_deleted",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .status()
            .expect("run the test copy");
        assert!(
            status.success(),
            "the deleted copy could not start its worker"
        );
    }

    /// A check's reply with these cached thumbnails and this poster.
    fn checked(cached: Vec<CachedThumbnail>, preview: Option<RawPreview>) -> Checked {
        Checked {
            size: 42,
            identity: kettle_media::PathIdentity {
                dev: 7,
                ino: 9,
                size: 42,
                mtime_seconds: -3,
                mtime_nanos: 999_999_999,
            },
            fingerprint: [5; 32],
            cached,
            preview,
        }
    }

    fn cached_at(name: &str, ino: u64) -> CachedThumbnail {
        CachedThumbnail {
            path: std::env::current_dir().unwrap().join(name),
            dev: 7,
            ino,
        }
    }

    /// The check's reply comes back exactly as it was sent, and nothing
    /// longer, larger or malformed decodes.
    #[test]
    fn the_check_s_reply_is_exact_and_bounded() {
        let preview = RawPreview {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 255],
        };
        let full = checked(
            (0..MAX_CACHED_THUMBNAILS as u64)
                .map(|ino| cached_at("large.png", ino))
                .collect(),
            Some(preview),
        );
        let encoded = encode_checked(&full).unwrap();
        assert_eq!(decode_checked(&encoded), Some(full));
        let bare = checked(Vec::new(), None);
        assert_eq!(decode_checked(&encode_checked(&bare).unwrap()), Some(bare));

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(decode_checked(&trailing), None);
        assert_eq!(decode_checked(&encoded[..encoded.len() - 1]), None);
        let mut magic = encoded.clone();
        magic[7] ^= 1;
        assert_eq!(decode_checked(&magic), None);

        let too_many = checked(
            (0..=MAX_CACHED_THUMBNAILS as u64)
                .map(|ino| cached_at("large.png", ino))
                .collect(),
            None,
        );
        assert!(encode_checked(&too_many).is_none());
        // One more well-formed thumbnail than a check names.
        let four = checked(
            (0..MAX_CACHED_THUMBNAILS as u64)
                .map(|ino| cached_at("large.png", ino))
                .collect(),
            None,
        );
        let mut counted = encode_checked(&four).unwrap();
        let one = encode_checked(&checked(vec![cached_at("large.png", 9)], None)).unwrap();
        let entry = &one[OUTPUT_MAGIC.len() + 69..one.len() - 12];
        let at = counted.len() - 12;
        counted.splice(at..at, entry.iter().copied());
        counted[OUTPUT_MAGIC.len() + 68] = MAX_CACHED_THUMBNAILS as u8 + 1;
        assert_eq!(decode_checked(&counted), None);
        counted[OUTPUT_MAGIC.len() + 68] = MAX_CACHED_THUMBNAILS as u8;
        assert_eq!(decode_checked(&counted), None, "the count must cover them");
        let mut late = encode_checked(&bare_check()).unwrap();
        late[OUTPUT_MAGIC.len() + 32..OUTPUT_MAGIC.len() + 36]
            .copy_from_slice(&1_000_000_000_u32.to_le_bytes());
        assert_eq!(decode_checked(&late), None);
        assert!(
            encode_checked(&checked(
                Vec::new(),
                Some(RawPreview {
                    width: MAX_PREVIEW_WIDTH + 1,
                    height: 1,
                    rgba: vec![0; (MAX_PREVIEW_WIDTH as usize + 1) * 4],
                }),
            ))
            .is_none()
        );
        let long = PathBuf::from(format!("/{}", "a".repeat(MAX_PATH_BYTES)));
        assert!(
            encode_checked(&checked(
                vec![CachedThumbnail {
                    path: long,
                    dev: 1,
                    ino: 1
                }],
                None
            ))
            .is_none()
        );
    }

    fn bare_check() -> Checked {
        checked(Vec::new(), None)
    }

    /// A second check's reply that agrees with `first`.
    #[cfg(target_os = "linux")]
    fn checked_again(first: &Checked) -> Checked {
        Checked {
            size: first.size,
            identity: first.identity,
            fingerprint: first.fingerprint,
            cached: Vec::new(),
            preview: None,
        }
    }

    /// The longest reply a check can send fits the bound the parent reads
    /// to, which would otherwise cut it short and lose the receipt.
    #[test]
    fn the_longest_reply_fits_what_the_parent_reads() {
        // Exactly the longest path a frame carries, in its own encoding: a
        // byte a character on Unix, two on Windows.
        let units = if cfg!(windows) {
            MAX_PATH_BYTES / 2
        } else {
            MAX_PATH_BYTES
        };
        let longest = PathBuf::from(format!("/{}", "a".repeat(units - 1)));
        assert_eq!(path_bytes(&longest).len(), MAX_PATH_BYTES);
        let reply = checked(
            (0..MAX_CACHED_THUMBNAILS as u64)
                .map(|ino| CachedThumbnail {
                    path: longest.clone(),
                    dev: 1,
                    ino,
                })
                .collect(),
            Some(RawPreview {
                width: MAX_PREVIEW_WIDTH,
                height: MAX_PREVIEW_HEIGHT,
                rgba: vec![255; MAX_PREVIEW_BYTES],
            }),
        );
        let encoded = encode_checked(&reply).unwrap();
        assert_eq!(encoded.len(), MAX_OUTPUT_BYTES);
        let read = read_bounded_preview(&mut encoded.as_slice()).unwrap();
        assert_eq!(decode_checked(&read), Some(reply));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_posters_are_opaque_for_the_straight_alpha_renderer() {
        let mut rgba = [20, 40, 60, 0, 10, 20, 30, 128];
        force_opaque_alpha(&mut rgba);
        assert_eq!(rgba, [20, 40, 60, 255, 10, 20, 30, 255]);
    }

    #[test]
    fn oversized_file_lists_are_rejected_before_scanning() {
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let paths = vec![path; MAX_FILE_LIST_ENTRIES + 1];
        assert!(
            VideoPasteCandidate::from_user_paths(&paths, VideoPasteSource::Clipboard).is_none()
        );
    }

    /// A private cache directory holding the video's thumbnail at `classes`,
    /// each a few bytes the check never parses.
    #[cfg(target_os = "linux")]
    fn thumbnail_cache(cache: &Path, video: &Path, classes: &[&str]) -> Vec<PathBuf> {
        use md5::{Digest as _, Md5};
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        let uri = url::Url::from_file_path(video).unwrap().to_string();
        let digest = thumbnail_digest_hex(Md5::digest(uri.as_bytes()).into());
        classes
            .iter()
            .map(|class| {
                let directory = cache.join("thumbnails").join(class);
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
                let path = directory.join(format!("{digest}.png"));
                std::fs::write(&path, class.as_bytes()).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
                path
            })
            .collect()
    }

    /// The check names the video's cached thumbnails, largest size first,
    /// each with the file it opened, and only those opened through a trusted
    /// chain: never a link, a file another user could change, or one in a
    /// directory another user could swap.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_check_names_only_trusted_cached_thumbnails() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

        let dir = crate::test_tempdir();
        let video = dir.path().join("clip one.mp4");
        write_test_video(&video, b"fixture");
        let cache = dir.path().join("cache");
        let paths = thumbnail_cache(&cache, &video, &["normal", "xx-large", "large"]);
        let named = cached_thumbnails_in(&cache, &video);
        let expected: Vec<_> = [&paths[1], &paths[2], &paths[0]]
            .into_iter()
            .map(|path| {
                let metadata = std::fs::metadata(path).unwrap();
                CachedThumbnail {
                    path: path.clone(),
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                }
            })
            .collect();
        assert_eq!(named, expected);
        assert!(cached_thumbnails_in(&cache, &dir.path().join("other.mp4")).is_empty());

        // Readable by all is how caches are written; writable by others is not.
        std::fs::set_permissions(&paths[1], std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(cached_thumbnails_in(&cache, &video).len(), 3);
        std::fs::set_permissions(&paths[1], std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(cached_thumbnails_in(&cache, &video), expected[1..]);

        std::fs::remove_file(&paths[2]).unwrap();
        symlink(&paths[0], &paths[2]).unwrap();
        assert_eq!(cached_thumbnails_in(&cache, &video), expected[2..]);

        let shared = dir.path().join("shared");
        thumbnail_cache(&shared, &video, &["normal"]);
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            cached_thumbnails_in(&shared, &video).is_empty(),
            "a private leaf cannot make a writable cache ancestor trustworthy"
        );
    }

    /// The check looks in `XDG_CACHE_HOME` when it is set.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_check_looks_in_the_user_s_thumbnail_cache() {
        const CHILD: &str = "KETTLE_VIDEO_PREVIEW_LINUX_CACHE_CHILD";
        const VIDEO: &str = "KETTLE_VIDEO_PREVIEW_LINUX_CACHE_VIDEO";
        if std::env::var_os(CHILD).is_none() {
            let dir = crate::test_tempdir();
            let video = dir.path().join("poster.mp4");
            write_test_video(&video, VIDEO_FIXTURE);
            thumbnail_cache(dir.path(), &video, &["large"]);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "video_preview::tests::the_check_looks_in_the_user_s_thumbnail_cache",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env(VIDEO, &video)
                .env("XDG_CACHE_HOME", dir.path())
                .status()
                .expect("re-exec the cache lookup test");
            assert!(status.success(), "the cache lookup child failed: {status}");
            return;
        }
        let video = PathBuf::from(std::env::var_os(VIDEO).expect("video fixture path"));
        let named = cached_thumbnails(&video);
        assert_eq!(named.len(), 1, "{named:?}");
        assert!(
            named[0]
                .path
                .parent()
                .unwrap()
                .ends_with("thumbnails/large"),
            "{named:?}"
        );
    }

    /// A worker reply of `kind`, rendered from the file `identity` names.
    #[cfg(unix)]
    fn worker_reply(
        kind: kettle_media::MediaKind,
        identity: Option<kettle_media::PathIdentity>,
    ) -> Result<kettle_media::RenderOutput, kettle_media::client::RenderError> {
        let whole = kettle_media::Crop {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        Ok(kettle_media::RenderOutput {
            kind,
            rendered: kettle_media::Rendered {
                width: 2,
                height: 1,
                rgba: vec![10, 20, 30, 255, 40, 50, 60, 255],
                digest: kettle_media::Digest {
                    sha256: [0; 32],
                    path_identity: identity,
                },
                layout: kettle_media::RenderLayout {
                    source_width: 2.0,
                    source_height: 1.0,
                    image_in_target: whole,
                    result_in_target: whole,
                },
                exact_source: None,
                source_text: Vec::new(),
                fence_sources: Vec::new(),
                fence_count: 0,
                fence_index: None,
                uncovered_scripts: Vec::new(),
                warnings: Vec::new(),
                video: None,
            },
        })
    }

    #[cfg(unix)]
    fn worker_failure(
        code: kettle_media::FailureCode,
    ) -> Result<kettle_media::RenderOutput, kettle_media::client::RenderError> {
        Err(kettle_media::client::RenderError::Failure(code))
    }

    /// The poster `worker_poster` makes for `checked` of `path` when the
    /// worker answers `replies` in turn and a second check finds the file
    /// unchanged, and the jobs it sent.
    #[cfg(unix)]
    fn poster_from(
        path: &Path,
        checked: &Checked,
        replies: Vec<Result<kettle_media::RenderOutput, kettle_media::client::RenderError>>,
    ) -> (Option<kettle_core::ImageData>, Vec<kettle_media::Job>) {
        poster_rechecked(
            path,
            checked,
            replies,
            PreviewChildOutcome::Ready(bare_check()),
        )
    }

    /// [`poster_from`], the second check answering `again`. Every render is
    /// held to the receipt's deadline.
    #[cfg(unix)]
    fn poster_rechecked(
        path: &Path,
        checked: &Checked,
        replies: Vec<Result<kettle_media::RenderOutput, kettle_media::client::RenderError>>,
        again: PreviewChildOutcome<Checked>,
    ) -> (Option<kettle_core::ImageData>, Vec<kettle_media::Job>) {
        let deadline = std::time::Instant::now() + RECEIPT_DEADLINE;
        let mut replies = std::collections::VecDeque::from(replies);
        let mut jobs = Vec::new();
        let image = worker_poster(
            path,
            checked,
            deadline,
            |job, control| {
                assert_eq!(control.deadline(), Some(deadline));
                jobs.push(job.clone());
                replies.pop_front().expect("a job past the replies")
            },
            || again,
        )
        .expect("no lost child");
        assert!(replies.is_empty(), "replies left over: {replies:?}");
        (image, jobs)
    }

    #[cfg(unix)]
    fn held_to(path: &Path, dev: u64, ino: u64) -> kettle_media::Source {
        kettle_media::Source::Path {
            path: kettle_media::NativePath::from_path(path).unwrap(),
            authorization: kettle_media::Authorization::ExternalAttested(
                kettle_media::ExternalAttested { dev, ino },
            ),
        }
    }

    /// The poster is the worker's Auto render of the checked file, held to
    /// the inode the check opened and fitted inside the receipt's bounds.
    #[cfg(unix)]
    #[test]
    fn a_poster_comes_from_the_worker_held_to_the_checked_file() {
        use kettle_media::MediaKind;
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let checked = bare_check();
        let (image, jobs) = poster_from(
            &path,
            &checked,
            vec![worker_reply(MediaKind::Video, Some(checked.identity))],
        );
        let image = image.expect("the worker's poster");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(&image.rgba[..], &[10, 20, 30, 255, 40, 50, 60, 255]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, kettle_media::JobKind::Auto);
        assert_eq!(jobs[0].source, held_to(&path, 7, 9));
        assert_eq!(
            jobs[0].target,
            kettle_media::Target {
                width: MAX_PREVIEW_WIDTH,
                height: MAX_PREVIEW_HEIGHT,
                scale: 1.0,
                crop: None,
            }
        );
        assert!(jobs[0].fallback_fonts.is_empty());
    }

    /// A poster is kept only when a second check finds the same file with
    /// the same contents: one rewritten in place with its time restored, one
    /// replaced, or one the check now refuses gets none.
    #[cfg(unix)]
    #[test]
    fn a_poster_needs_a_second_check_to_agree() {
        use kettle_media::MediaKind;
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let checked = bare_check();
        let poster = || vec![worker_reply(MediaKind::Video, Some(checked.identity))];
        let rewritten = Checked {
            fingerprint: [6; 32],
            ..bare_check()
        };
        let replaced = Checked {
            identity: kettle_media::PathIdentity {
                ino: 10,
                ..checked.identity
            },
            ..bare_check()
        };
        for again in [
            PreviewChildOutcome::Ready(rewritten),
            PreviewChildOutcome::Ready(replaced),
            PreviewChildOutcome::Failed,
        ] {
            let (image, jobs) = poster_rechecked(&path, &checked, poster(), again);
            assert!(image.is_none());
            assert_eq!(jobs.len(), 1);
        }
        let (image, _) = poster_rechecked(
            &path,
            &checked,
            poster(),
            PreviewChildOutcome::Ready(bare_check()),
        );
        assert!(image.is_some());
    }

    /// A poster past the receipt's bounds is none, whatever sent it.
    #[cfg(unix)]
    #[test]
    fn a_poster_past_the_receipt_s_bounds_is_none() {
        use kettle_media::MediaKind;
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let checked = bare_check();
        let mut wide = worker_reply(MediaKind::Video, Some(checked.identity)).unwrap();
        wide.rendered.width = MAX_PREVIEW_WIDTH + 1;
        wide.rendered.rgba = vec![255; (MAX_PREVIEW_WIDTH as usize + 1) * 4];
        let (image, jobs) = poster_from(&path, &checked, vec![Ok(wide)]);
        assert!(image.is_none());
        assert_eq!(jobs.len(), 1);
    }

    /// A render of another file (changed since the check) or of something
    /// other than a video is no poster, and nothing else is tried.
    #[cfg(unix)]
    #[test]
    fn a_render_of_another_file_or_kind_is_no_poster() {
        use kettle_media::MediaKind;
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let checked = checked(vec![cached_at("large.png", 11)], None);
        let changed = kettle_media::PathIdentity {
            mtime_nanos: 0,
            ..checked.identity
        };
        for reply in [
            worker_reply(MediaKind::Video, Some(changed)),
            worker_reply(MediaKind::Video, None),
            worker_reply(MediaKind::Raster, Some(checked.identity)),
        ] {
            let (image, jobs) = poster_from(&path, &checked, vec![reply]);
            assert!(image.is_none());
            assert_eq!(jobs.len(), 1);
        }
    }

    /// Where no decoder could make a poster, Linux tries the checked cached
    /// thumbnails in turn, each held to the file the check opened and to the
    /// video's URI and time as the check saw them, until one renders.
    #[cfg(target_os = "linux")]
    #[test]
    fn without_a_decoder_a_cached_thumbnail_stands_in() {
        use kettle_media::{FailureCode, JobKind, MediaKind, ThumbnailOf};
        let path = std::env::current_dir().unwrap().join("clip one.mp4");
        let cached = vec![
            cached_at("xx-large.png", 11),
            cached_at("large.png", 12),
            cached_at("normal.png", 13),
        ];
        let checked = checked(cached, None);
        let of = ThumbnailOf {
            uri_sha256: ThumbnailOf::uri_digest(
                &url::Url::from_file_path(&path).unwrap().to_string(),
            ),
            mtime_seconds: -3,
            mtime_nanos: 999_999_999,
        };
        for failure in [
            FailureCode::BackendUnavailable,
            FailureCode::CodecUnavailable,
            FailureCode::UnsupportedContainer,
            FailureCode::SandboxUnavailable,
            FailureCode::RenderParse,
            FailureCode::RenderResource,
        ] {
            let (image, jobs) = poster_from(
                &path,
                &checked,
                vec![
                    worker_failure(failure),
                    worker_failure(FailureCode::Changed),
                    worker_reply(MediaKind::Raster, None),
                ],
            );
            assert!(image.is_some(), "{failure:?}");
            assert_eq!(jobs.len(), 3, "{failure:?}");
            for (job, cached) in jobs[1..].iter().zip(&checked.cached) {
                assert_eq!(job.kind, JobKind::CachedThumbnail(of));
                assert_eq!(job.source, held_to(&cached.path, cached.dev, cached.ino));
            }
        }
        // Nothing renders: no poster, every thumbnail tried once.
        let (image, jobs) = poster_from(
            &path,
            &checked,
            vec![
                worker_failure(FailureCode::BackendUnavailable),
                worker_failure(FailureCode::Changed),
                worker_failure(FailureCode::UnsupportedMedia),
                worker_failure(FailureCode::Changed),
            ],
        );
        assert!(image.is_none());
        assert_eq!(jobs.len(), 4);
        // A video rewritten before the worker read it fails without saying
        // so; the second check sees its new time and drops the stand-in.
        let (image, jobs) = poster_rechecked(
            &path,
            &checked,
            vec![
                worker_failure(FailureCode::BackendUnavailable),
                worker_reply(MediaKind::Raster, None),
            ],
            PreviewChildOutcome::Ready(Checked {
                identity: kettle_media::PathIdentity {
                    mtime_seconds: 5,
                    ..checked.identity
                },
                ..checked_again(&checked)
            }),
        );
        assert!(image.is_none());
        assert_eq!(jobs.len(), 2);
        // A deadline that passes stops the search.
        let (image, jobs) = poster_from(
            &path,
            &checked,
            vec![
                worker_failure(FailureCode::BackendUnavailable),
                worker_failure(FailureCode::RenderTimeout),
            ],
        );
        assert!(image.is_none());
        assert_eq!(jobs.len(), 2);
    }

    /// A video that changed, a deadline that passed, a worker that is missing
    /// or a file that is gone gets no cached stand-in; nor does any failure
    /// on macOS, which has no cache to fall back to.
    #[cfg(unix)]
    #[test]
    fn other_failures_get_no_stand_in() {
        use kettle_media::FailureCode;
        let path = std::env::current_dir().unwrap().join("clip.mp4");
        let checked = checked(vec![cached_at("large.png", 11)], None);
        let mut failures = vec![
            worker_failure(FailureCode::Changed),
            worker_failure(FailureCode::RenderTimeout),
            worker_failure(FailureCode::WorkerUnavailable),
            worker_failure(FailureCode::FileNotFound),
            Err(kettle_media::client::RenderError::Cancelled),
        ];
        if cfg!(target_os = "macos") {
            failures.push(worker_failure(FailureCode::BackendUnavailable));
        }
        for failure in failures {
            let (image, jobs) = poster_from(&path, &checked, vec![failure]);
            assert!(image.is_none());
            assert_eq!(jobs.len(), 1);
        }
    }

    #[test]
    fn thumbnail_digest_keeps_leading_zeroes_and_lowercase_hex() {
        assert_eq!(
            thumbnail_digest_hex([
                0x0c, 0xc1, 0x75, 0xb9, 0xc0, 0xf1, 0xb6, 0xa8, 0x31, 0xc3, 0x99, 0xe2, 0x69, 0x77,
                0x26, 0x61,
            ]),
            "0cc175b9c0f1b6a831c399e269772661"
        );
        assert_eq!(
            thumbnail_digest_hex([0; 16]),
            "00000000000000000000000000000000"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn thumbnail_md5_matches_the_standard_test_vector() {
        use md5::{Digest as _, Md5};
        assert_eq!(
            thumbnail_digest_hex(Md5::digest(b"a").into()),
            "0cc175b9c0f1b6a831c399e269772661"
        );
    }
}
