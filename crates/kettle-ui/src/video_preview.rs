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
const OUTPUT_MAGIC: &[u8; 8] = b"KTLVPOU1";
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
/// One surviving thread may drain every queued job before reaching this one.
/// Two seconds of bounded slack cover dispatch overhead. An unusually loaded
/// host drops the optional receipt instead of retaining pending state forever.
pub(crate) const PENDING_RECEIPT_TIMEOUT: Duration = Duration::from_secs(
    ((PREVIEW_QUEUE_CAPACITY + 1) as u64 * WORKER_TIMEOUT.as_secs() * MAX_WORKER_ATTEMPTS as u64)
        + 2,
);
const MAX_FILE_LIST_ENTRIES: usize = 256;
const FINGERPRINT_SAMPLE_BYTES: usize = 64 * 1024;
#[cfg(target_os = "linux")]
const MAX_CACHED_THUMBNAIL_DIMENSION: u32 = 4_096;
#[cfg(target_os = "linux")]
const MAX_CACHED_THUMBNAIL_PIXELS: u64 = 16 * 1024 * 1024;

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
    pub(crate) fn new(proxy: winit::event_loop::EventLoopProxy<crate::app::UserEvent>) -> Self {
        let (jobs, receiver) = crossbeam_channel::bounded::<PreviewJob>(PREVIEW_QUEUE_CAPACITY);
        let live_workers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for index in 0..PREVIEW_THREAD_COUNT {
            let receiver = receiver.clone();
            let proxy = proxy.clone();
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
                        let (candidate, preview) = match run_preview_child(job.request.path()) {
                            PreviewChildOutcome::Ready((size, preview)) => (
                                Some(VideoPasteCandidate::from_verified(job.request, size)),
                                preview,
                            ),
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

fn retry_preview_timeout<T>(
    mut attempt: impl FnMut() -> PreviewChildAttempt<T>,
) -> PreviewChildOutcome<T> {
    for _ in 0..MAX_WORKER_ATTEMPTS {
        match attempt() {
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
        .take((MAX_PREVIEW_BYTES + 64) as u64)
        .read_to_end(&mut output)?;
    Ok(output)
}

fn stop_and_reap_child(child: &mut std::process::Child) -> bool {
    match child.kill() {
        Ok(()) => child.wait().is_ok(),
        Err(_) => matches!(child.try_wait(), Ok(Some(_))),
    }
}

fn run_preview_child(path: &Path) -> PreviewChildOutcome<(u64, Option<kettle_core::ImageData>)> {
    retry_preview_timeout(|| run_preview_child_once(path))
}

fn run_preview_child_once(
    path: &Path,
) -> PreviewChildAttempt<(u64, Option<kettle_core::ImageData>)> {
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
    let Some(mut stdin) = child.stdin.take() else {
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
    if stdin.write_all(&input).is_err() {
        return if stop_and_reap_child(&mut child) {
            PreviewChildAttempt::Failed
        } else {
            PreviewChildAttempt::WorkerLost
        };
    }
    drop(stdin);

    let Some(mut stdout) = child.stdout.take() else {
        return if stop_and_reap_child(&mut child) {
            PreviewChildAttempt::Failed
        } else {
            PreviewChildAttempt::WorkerLost
        };
    };
    let reader = match std::thread::Builder::new()
        .name("kettle-video-preview-reader".to_owned())
        .spawn(move || read_bounded_preview(&mut stdout))
    {
        Ok(reader) => reader,
        Err(error) => {
            log::warn!("video preview reader could not start: {error}");
            return if stop_and_reap_child(&mut child) {
                PreviewChildAttempt::Failed
            } else {
                PreviewChildAttempt::WorkerLost
            };
        }
    };
    let deadline = std::time::Instant::now() + WORKER_TIMEOUT;
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
                return match reader.join() {
                    Ok(Ok(_)) => PreviewChildAttempt::TimedOut,
                    Ok(Err(_)) | Err(_) => PreviewChildAttempt::Failed,
                };
            }
            Err(_) => {
                if !stop_and_reap_child(&mut child) {
                    return PreviewChildAttempt::WorkerLost;
                }
                // Join only to bound the reader thread. Whatever it returns,
                // the failed wait already makes this a non-retryable failure.
                let _ = reader.join();
                return PreviewChildAttempt::Failed;
            }
        }
    };
    let Ok(Ok(output)) = reader.join() else {
        return PreviewChildAttempt::Failed;
    };
    if !status.success() {
        return if worker_exit_is_retryable(status.code()) {
            PreviewChildAttempt::TimedOut
        } else if status.code() == Some(WORKER_SKEW_EXIT) {
            PreviewChildAttempt::Skewed
        } else {
            PreviewChildAttempt::Failed
        };
    }
    match decode_preview(&output) {
        Some(output) => PreviewChildAttempt::Ready(output),
        None => PreviewChildAttempt::Failed,
    }
}

fn encode_request(path: &Path, build: &str) -> Option<Vec<u8>> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let bytes = {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
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
    let bytes = &input[path_start..];
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt as _;
        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    };
    #[cfg(windows)]
    let path = {
        use std::os::windows::ffi::OsStringExt as _;
        if len & 1 != 0 {
            return Err(RequestError::Malformed);
        }
        let wide = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();
        PathBuf::from(OsString::from_wide(&wide))
    };
    Ok(path)
}

fn encode_preview(size: u64, preview: Option<&RawPreview>) -> Option<Vec<u8>> {
    let (width, height, rgba) = if let Some(preview) = preview {
        if !valid_preview(preview.width, preview.height, preview.rgba.len()) {
            return None;
        }
        (preview.width, preview.height, preview.rgba.as_slice())
    } else {
        (0, 0, &[][..])
    };
    let mut out = Vec::with_capacity(28 + rgba.len());
    out.extend_from_slice(OUTPUT_MAGIC);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
    out.extend_from_slice(rgba);
    Some(out)
}

fn decode_preview(output: &[u8]) -> Option<(u64, Option<kettle_core::ImageData>)> {
    if output.len() < 28 || &output[..8] != OUTPUT_MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(output[8..16].try_into().ok()?);
    let width = u32::from_le_bytes(output[16..20].try_into().ok()?);
    let height = u32::from_le_bytes(output[20..24].try_into().ok()?);
    let len = u32::from_le_bytes(output[24..28].try_into().ok()?) as usize;
    if output.len() != 28 + len {
        return None;
    }
    if width == 0 && height == 0 && len == 0 {
        return Some((size, None));
    }
    if !valid_preview(width, height, len) {
        return None;
    }
    Some((
        size,
        Some(kettle_core::ImageData::new(
            width,
            height,
            output[28..].to_vec(),
        )?),
    ))
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
    // Keep the user-visible absolute path for the platform APIs. In particular,
    // `SHCreateItemFromParsingName` consumes a Shell parsing name, not the
    // extended-length canonical path used for identity. The identity checks
    // before and after extraction still reject path swaps. Source: Microsoft
    // `SHCreateItemFromParsingName` API documentation.
    let preview = platform_thumbnail(&path);
    if !file_identity_matches(&path, &identity, &retained) {
        return 5;
    }
    let Some(output) = encode_preview(identity.len, preview.as_ref()) else {
        return 6;
    };
    if std::io::stdout().write_all(&output).is_err() {
        return 7;
    }
    0
}

#[cfg(target_os = "macos")]
fn platform_thumbnail(path: &Path) -> Option<RawPreview> {
    use block2_06::RcBlock;
    use objc2_06::AnyThread as _;
    use objc2_core_foundation_03::CGSize;
    use objc2_foundation_03::NSURL;
    use objc2_quick_look_thumbnailing::{
        QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes,
        QLThumbnailGenerator, QLThumbnailRepresentation,
    };
    use std::os::unix::ffi::OsStrExt as _;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let pointer = std::ptr::NonNull::new(c_path.as_ptr().cast_mut())?;
    let url = unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(pointer, false, None)
    };
    let request = unsafe {
        QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
            QLThumbnailGenerationRequest::alloc(),
            &url,
            CGSize::new(MAX_PREVIEW_WIDTH as f64, MAX_PREVIEW_HEIGHT as f64),
            1.0,
            QLThumbnailGenerationRequestRepresentationTypes::Thumbnail,
        )
    };
    unsafe { request.setIconMode(false) };
    let generator = unsafe { QLThumbnailGenerator::sharedGenerator() };
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    // Apple's QLThumbnailGenerator
    // `generateBestRepresentation(for:completion:)` contract imports the
    // completion as `@escaping` and guarantees that it is called when the
    // request finishes. Own the block on the heap across that asynchronous
    // boundary; the worker still applies its bounded receive deadline.
    let completion = RcBlock::new(
        move |representation: *mut QLThumbnailRepresentation,
              _error: *mut objc2_foundation_03::NSError| {
            let preview = unsafe { representation.as_ref() }.and_then(|representation| {
                let image = unsafe { representation.CGImage() };
                cg_image_to_rgba(&image)
            });
            let _ = sender.send(preview);
        },
    );
    unsafe {
        generator.generateBestRepresentationForRequest_completionHandler(&request, &completion)
    };
    receiver.recv_timeout(Duration::from_millis(1_500)).ok()?
}

#[cfg(target_os = "macos")]
fn cg_image_to_rgba(image: &objc2_core_graphics_03::CGImage) -> Option<RawPreview> {
    use objc2_core_foundation_03::{CGPoint, CGRect, CGSize};
    use objc2_core_graphics_03::{
        CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext, CGImageAlphaInfo,
        CGImageByteOrderInfo,
    };

    let source_width = objc2_core_graphics_03::CGImage::width(Some(image));
    let source_height = objc2_core_graphics_03::CGImage::height(Some(image));
    if source_width == 0 || source_height == 0 {
        return None;
    }
    let scale = (MAX_PREVIEW_WIDTH as f64 / source_width as f64)
        .min(MAX_PREVIEW_HEIGHT as f64 / source_height as f64)
        .min(1.0);
    let width = (source_width as f64 * scale).round().max(1.0) as u32;
    let height = (source_height as f64 * scale).round().max(1.0) as u32;
    let row_bytes = width as usize * 4;
    let mut rgba = vec![0u8; row_bytes * height as usize];
    let color_space = CGColorSpace::new_device_rgb()?;
    // The renderer consumes straight RGBA and premultiplies in its shader.
    // Draw Quick Look's potentially translucent poster onto an opaque black
    // destination so it is not premultiplied a second time downstream.
    let bitmap_info =
        CGBitmapInfo(CGImageAlphaInfo::NoneSkipLast.0 | CGImageByteOrderInfo::Order32Big.0);
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width as usize,
            height as usize,
            8,
            row_bytes,
            Some(&color_space),
            bitmap_info.0,
        )?
    };
    CGContext::translate_ctm(Some(&context), 0.0, height as f64);
    CGContext::scale_ctm(Some(&context), 1.0, -1.0);
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(width as f64, height as f64),
        ),
        Some(image),
    );
    force_opaque_alpha(&mut rgba);
    Some(RawPreview {
        width,
        height,
        rgba,
    })
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

#[cfg(target_os = "linux")]
fn platform_thumbnail(path: &Path) -> Option<RawPreview> {
    use md5::{Digest as _, Md5};
    use std::os::unix::fs::MetadataExt as _;

    // Freedesktop.org's `Thumbnail Managing Standard`, sections "Thumbnail
    // URI" and "Thumbnail Creation", define the MD5 file name, cache classes,
    // and `Thumb::URI` / `Thumb::MTime` validation used below.
    let uri = url::Url::from_file_path(path).ok()?.to_string();
    let digest = thumbnail_digest_hex(Md5::digest(uri.as_bytes()).into());
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let metadata = std::fs::metadata(path).ok()?;
    let source_mtime = (metadata.mtime(), metadata.mtime_nsec());
    for class in ["xx-large", "x-large", "large", "normal"] {
        let candidate = cache
            .join("thumbnails")
            .join(class)
            .join(format!("{digest}.png"));
        if let Some(preview) = load_linux_cached_thumbnail(&candidate, &uri, source_mtime) {
            return Some(preview);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn load_linux_cached_thumbnail(path: &Path, uri: &str, mtime: (i64, i64)) -> Option<RawPreview> {
    // Decode the exact leaf opened through kettle-state's held, trusted parent
    // chain. Leaf-only O_NOFOLLOW still lets a writable cache ancestor swap a
    // directory or redirect an intermediate symlink before open.
    let mut file = kettle_state::open_trusted_file_read(path).ok()?;
    if !thumbnail_metadata_matches(&mut file, uri, mtime) {
        return None;
    }
    file.seek(std::io::SeekFrom::Start(0)).ok()?;
    let image = image::load(std::io::BufReader::new(file), image::ImageFormat::Png)
        .ok()?
        .thumbnail(MAX_PREVIEW_WIDTH, MAX_PREVIEW_HEIGHT)
        .to_rgba8();
    Some(RawPreview {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

#[cfg(target_os = "linux")]
fn thumbnail_metadata_matches(file: &mut std::fs::File, uri: &str, mtime: (i64, i64)) -> bool {
    if file.seek(std::io::SeekFrom::Start(0)).is_err() {
        return false;
    }
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let reader = match decoder.read_info() {
        Ok(reader) => reader,
        Err(_) => return false,
    };
    let info = reader.info();
    if !cached_thumbnail_dimensions_allowed(info.width, info.height) {
        return false;
    }
    let text = |key: &str| {
        info.uncompressed_latin1_text
            .iter()
            .find(|chunk| chunk.keyword == key)
            .map(|chunk| chunk.text.as_str())
    };
    text("Thumb::URI") == Some(uri)
        && text("Thumb::MTime").is_some_and(|recorded| thumbnail_mtime_matches(recorded, mtime))
}

/// Whether a thumbnail's recorded `Thumb::MTime` is the source's modification
/// time, `(seconds, nanoseconds)`. The standard writes whole seconds; tumbler
/// adds a fraction (`1696300000.123456`). Either is the source's time when it
/// agrees with the seconds and nanoseconds to the recorded precision,
/// truncated or rounded, a rounding that carries into the next second
/// included. Anything else (another time, signs, exponents, spaces, more than
/// nine fraction digits, a fraction before 1970, where the sign makes it
/// ambiguous) is stale.
#[cfg(any(test, target_os = "linux"))]
fn thumbnail_mtime_matches(recorded: &str, (seconds, nanos): (i64, i64)) -> bool {
    let Some((whole, fraction)) = recorded.split_once('.') else {
        return recorded == seconds.to_string();
    };
    if seconds < 0
        || fraction.is_empty()
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    let (Ok(recorded_seconds), Ok(value)) = (whole.parse::<i64>(), fraction.parse::<i64>()) else {
        return false;
    };
    // Exactly the digits an integer prints: no sign, no leading zero.
    if whole != recorded_seconds.to_string() {
        return false;
    }
    // The fraction's unit, in nanoseconds: 100_000_000 for one digit.
    let digits = fraction.len() as u32;
    let unit = 10i64.pow(9 - digits);
    let truncated = (seconds, nanos / unit);
    let rounded = match (nanos + unit / 2) / unit {
        carry if carry == 10i64.pow(digits) => (seconds + 1, 0),
        rounded => (seconds, rounded),
    };
    (recorded_seconds, value) == truncated || (recorded_seconds, value) == rounded
}

#[cfg(target_os = "linux")]
fn cached_thumbnail_dimensions_allowed(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_CACHED_THUMBNAIL_DIMENSION
        && height <= MAX_CACHED_THUMBNAIL_DIMENSION
        && u64::from(width) * u64::from(height) <= MAX_CACHED_THUMBNAIL_PIXELS
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn force_opaque_alpha(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_thumbnail(_path: &Path) -> Option<RawPreview> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
            PENDING_RECEIPT_TIMEOUT
                >= WORKER_TIMEOUT * MAX_WORKER_ATTEMPTS * (PREVIEW_QUEUE_CAPACITY as u32 + 1)
                    + Duration::from_secs(2),
            "pending state must outlive a full queue drained by one surviving worker"
        );
    }

    #[test]
    fn preview_child_retries_only_one_timeout() {
        let mut calls = 0;
        let mut outcomes = [
            PreviewChildAttempt::TimedOut,
            PreviewChildAttempt::Ready(7_u8),
        ]
        .into_iter();
        let result = retry_preview_timeout(|| {
            calls += 1;
            outcomes.next().unwrap()
        });
        assert_eq!(result, PreviewChildOutcome::Ready(7));
        assert_eq!(calls, 2, "one cold timeout gets one fresh worker");

        calls = 0;
        let result = retry_preview_timeout(|| {
            calls += 1;
            PreviewChildAttempt::<u8>::Failed
        });
        assert_eq!(result, PreviewChildOutcome::Failed);
        assert_eq!(calls, 1, "non-timeout failures must stay fail-closed");

        calls = 0;
        let result = retry_preview_timeout(|| {
            calls += 1;
            PreviewChildAttempt::<u8>::TimedOut
        });
        assert_eq!(result, PreviewChildOutcome::Failed);
        assert_eq!(calls, 2, "repeated timeouts must remain bounded");

        calls = 0;
        let result = retry_preview_timeout(|| {
            calls += 1;
            PreviewChildAttempt::<u8>::WorkerLost
        });
        assert_eq!(result, PreviewChildOutcome::WorkerLost);
        assert_eq!(calls, 1, "an unreaped child poisons its queue worker");
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

        let frame = encode_preview(7, None).unwrap();
        let mut reader = CompleteThenError {
            bytes: std::io::Cursor::new(frame.clone()),
        };
        assert!(
            read_bounded_preview(&mut reader).is_err(),
            "partial success must not authenticate bytes from a failed pipe read"
        );
        assert!(decode_preview(&frame).is_some(), "fixture must be valid");
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
                run_preview_child_once(&path),
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
        let result = retry_preview_timeout(|| {
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
        assert!(child.contains("status.code() == Some(WORKER_SKEW_EXIT)"));
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

    #[test]
    fn preview_protocol_requires_exact_bounded_rgba() {
        let preview = RawPreview {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 255],
        };
        let encoded = encode_preview(42, Some(&preview)).unwrap();
        let (size, decoded) = decode_preview(&encoded).expect("valid response");
        let decoded = decoded.expect("poster");
        assert_eq!(size, 42);
        assert_eq!((decoded.width, decoded.height), (2, 1));

        let metadata_only = encode_preview(7, None).unwrap();
        let (size, preview) = decode_preview(&metadata_only).expect("metadata-only response");
        assert_eq!(size, 7);
        assert!(preview.is_none());

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode_preview(&trailing).is_none());

        let invalid = RawPreview {
            width: MAX_PREVIEW_WIDTH + 1,
            height: 1,
            rgba: vec![0; (MAX_PREVIEW_WIDTH as usize + 1) * 4],
        };
        assert!(encode_preview(0, Some(&invalid)).is_none());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_cache_requires_owned_private_matching_png() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

        let dir = crate::test_tempdir();
        let video = dir.path().join("clip.mp4");
        write_test_video(&video, b"fixture");
        let uri = url::Url::from_file_path(&video).unwrap().to_string();
        let metadata = std::fs::metadata(&video).unwrap();
        let mtime = metadata.mtime().to_string();
        let source = (metadata.mtime(), metadata.mtime_nsec());
        let thumbnail = dir.path().join("thumbnail.png");
        {
            let file = std::fs::File::create(&thumbnail).unwrap();
            let mut encoder = png::Encoder::new(file, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_text_chunk("Thumb::URI".to_string(), uri.clone())
                .unwrap();
            encoder
                .add_text_chunk("Thumb::MTime".to_string(), mtime.clone())
                .unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[1, 2, 3, 255, 4, 5, 6, 255])
                .unwrap();
        }
        std::fs::set_permissions(&thumbnail, std::fs::Permissions::from_mode(0o600)).unwrap();

        let preview = load_linux_cached_thumbnail(&thumbnail, &uri, source)
            .expect("matching private thumbnail");
        assert_eq!((preview.width, preview.height), (256, 128));

        std::fs::set_permissions(&thumbnail, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load_linux_cached_thumbnail(&thumbnail, &uri, source).is_some(),
            "a conventional read-only thumbnail cache leaf is safe"
        );
        std::fs::set_permissions(&thumbnail, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(load_linux_cached_thumbnail(&thumbnail, &uri, source).is_none());
        std::fs::set_permissions(&thumbnail, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_linux_cached_thumbnail(&thumbnail, &uri, (0, 0)).is_none());

        let linked = dir.path().join("linked.png");
        symlink(&thumbnail, &linked).unwrap();
        assert!(load_linux_cached_thumbnail(&linked, &uri, source).is_none());

        let writable_parent = dir.path().join("writable-cache");
        std::fs::create_dir(&writable_parent).unwrap();
        std::fs::set_permissions(&writable_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let untrusted = writable_parent.join("thumbnail.png");
        std::fs::copy(&thumbnail, &untrusted).unwrap();
        std::fs::set_permissions(&untrusted, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            load_linux_cached_thumbnail(&untrusted, &uri, source).is_none(),
            "a private leaf cannot make a writable cache ancestor trustworthy"
        );

        // tumbler records the time with a fraction: the same source still
        // matches, another time or another URI does not.
        let fractional = |name: &str, uri: &str, recorded: &str| {
            let path = dir.path().join(name);
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = png::Encoder::new(file, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_text_chunk("Thumb::URI".to_string(), uri.to_string())
                .unwrap();
            encoder
                .add_text_chunk("Thumb::MTime".to_string(), recorded.to_string())
                .unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[1, 2, 3, 255, 4, 5, 6, 255])
                .unwrap();
            drop(writer);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            path
        };
        let micros = format!("{mtime}.{:06}", metadata.mtime_nsec() / 1_000);
        let tumbler = fractional("tumbler.png", &uri, &micros);
        assert!(load_linux_cached_thumbnail(&tumbler, &uri, source).is_some());
        let stale = fractional(
            "stale.png",
            &uri,
            &format!("{}.000001", metadata.mtime() - 1),
        );
        assert!(load_linux_cached_thumbnail(&stale, &uri, source).is_none());
        let other = fractional("other.png", "file:///tmp/other.mp4", &micros);
        assert!(load_linux_cached_thumbnail(&other, &uri, source).is_none());
    }

    /// A recorded `Thumb::MTime` is the source's time in whole seconds, or
    /// with a fraction (tumbler) that agrees with the nanoseconds to its own
    /// precision, truncated or rounded; nothing else matches.
    #[test]
    fn a_fractional_thumbnail_mtime_matches_the_same_source() {
        let source = (1_696_300_000, 123_456_789);
        for recorded in [
            "1696300000",
            "1696300000.1",
            "1696300000.12",
            "1696300000.123456",
            "1696300000.123457",
            "1696300000.123456789",
        ] {
            assert!(thumbnail_mtime_matches(recorded, source), "{recorded}");
        }
        for recorded in [
            "1696299999",
            "1696300001",
            "1696300000.2",
            "1696300000.123458",
            "1696300000.1234567890",
            "1696300000.",
            ".123",
            "+1696300000",
            "1696300000.12e3",
            " 1696300000",
            "1696300000.12 ",
            "01696300000",
            "1696300000.-1",
            "-1696300000.1",
        ] {
            assert!(!thumbnail_mtime_matches(recorded, source), "{recorded}");
        }
        // Rounding may carry into the next second.
        assert!(thumbnail_mtime_matches(
            "1696300001.000000",
            (1_696_300_000, 999_999_600)
        ));
        assert!(thumbnail_mtime_matches(
            "1696300000.999999",
            (1_696_300_000, 999_999_600)
        ));
        assert!(!thumbnail_mtime_matches(
            "1696300001.000000",
            (1_696_300_000, 999_999_000)
        ));
        // Before 1970 the sign makes a fraction ambiguous: whole seconds only.
        assert!(thumbnail_mtime_matches("-1", (-1, 250_000_000)));
        assert!(!thumbnail_mtime_matches("-1.250000", (-1, 250_000_000)));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_platform_adapter_resolves_the_freedesktop_cache_entry() {
        use md5::{Digest as _, Md5};
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

        const CHILD: &str = "KETTLE_VIDEO_PREVIEW_LINUX_ADAPTER_CHILD";
        const VIDEO: &str = "KETTLE_VIDEO_PREVIEW_LINUX_ADAPTER_VIDEO";
        if std::env::var_os(CHILD).is_none() {
            let dir = crate::test_tempdir();
            let video = dir.path().join("poster.mp4");
            write_test_video(&video, VIDEO_FIXTURE);
            let uri = url::Url::from_file_path(&video).unwrap().to_string();
            let mtime = std::fs::metadata(&video).unwrap().mtime().to_string();
            let digest = thumbnail_digest_hex(Md5::digest(uri.as_bytes()).into());
            let cache = dir.path().join("thumbnails/normal");
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&cache)
                .unwrap();
            let thumbnail = cache.join(format!("{digest}.png"));
            {
                let file = std::fs::File::create(&thumbnail).unwrap();
                let mut encoder = png::Encoder::new(file, 2, 1);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.add_text_chunk("Thumb::URI".into(), uri).unwrap();
                encoder
                    .add_text_chunk("Thumb::MTime".into(), mtime)
                    .unwrap();
                let mut writer = encoder.write_header().unwrap();
                writer
                    .write_image_data(&[1, 2, 3, 255, 4, 5, 6, 255])
                    .unwrap();
            }
            std::fs::set_permissions(&thumbnail, std::fs::Permissions::from_mode(0o600)).unwrap();

            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "video_preview::tests::linux_platform_adapter_resolves_the_freedesktop_cache_entry",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env(VIDEO, &video)
                .env("XDG_CACHE_HOME", dir.path())
                .status()
                .expect("re-exec Linux video adapter test");
            assert!(
                status.success(),
                "Linux video adapter child failed: {status}"
            );
            return;
        }

        let video = PathBuf::from(std::env::var_os(VIDEO).expect("video fixture path"));
        let preview = platform_thumbnail(&video).expect("Freedesktop video poster");
        assert_eq!((preview.width, preview.height), (256, 128));
        assert!(valid_preview(
            preview.width,
            preview.height,
            preview.rgba.len()
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_cache_rejects_unbounded_source_geometry() {
        assert!(cached_thumbnail_dimensions_allowed(1_024, 1_024));
        assert!(!cached_thumbnail_dimensions_allowed(0, 1));
        assert!(!cached_thumbnail_dimensions_allowed(4_097, 1));
        assert!(!cached_thumbnail_dimensions_allowed(4_096, 4_097));
    }
}
