use super::{GpuFault, Renderer, Result, latch_gpu_fault, srgb, srgb_encode};

/// A queued screenshot request. Phase 4 of the terminalshot design
/// consumes this in `render_frame`.
#[derive(Debug, Clone)]
pub struct ScreenshotRequest {
    /// Where to save the PNG.
    pub out_path: std::path::PathBuf,
    /// Whether the destination belongs to Kettle's private state or was
    /// explicitly selected by the user.
    pub output_policy: ScreenshotOutputPolicy,
    /// If `Some`, crop the captured frame to this pixel rect
    /// (the focused pane's geometry). If `None`, capture the
    /// whole window.
    pub crop: Option<(f32, f32, f32, f32)>,
    /// Optional completion signal for programmatic callers (`kettle ctl
    /// screenshot` / MCP). The UI action leaves this `None` and keeps its
    /// optimistic notification behavior.
    pub completion: Option<std::sync::mpsc::Sender<Result<std::path::PathBuf, String>>>,
    /// Cooperative cancellation for bounded control-plane callers. A timed-out
    /// request must neither keep the renderer BUSY nor publish a file later
    /// when a minimized window is restored. UI-triggered captures leave this
    /// `None` because their lifecycle is owned entirely by the renderer.
    pub cancellation: Option<ScreenshotCancellation>,
    /// Event-loop wake used only when a bounded GPU wait classifies the shared
    /// device as wedged. The worker runs off-thread; without this wake an idle
    /// application can remain asleep with the destroyed device installed.
    pub recovery_wake: Option<ScreenshotRecoveryWake>,
}

/// Cloneable, application-owned event-loop wake for screenshot GPU recovery.
#[derive(Clone)]
pub struct ScreenshotRecoveryWake(
    std::sync::Arc<dyn Fn() + Send + Sync + std::panic::UnwindSafe + 'static>,
);

impl std::fmt::Debug for ScreenshotRecoveryWake {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ScreenshotRecoveryWake(..)")
    }
}

impl ScreenshotRecoveryWake {
    pub fn new(wake: impl Fn() + Send + Sync + std::panic::UnwindSafe + 'static) -> Self {
        Self(std::sync::Arc::new(wake))
    }

    pub(crate) fn wake(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.0)()));
    }
}

/// Cloneable publication state shared by a control request and the renderer.
///
/// The final transition is a race between the caller cancelling and the worker
/// committing the fully encoded, durably flushed file. Whichever wins is irreversible:
/// a timeout that successfully cancels cannot publish later, while a worker
/// that has already committed must report its real completion instead of a
/// contradictory timeout.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotCancellation(std::sync::Arc<std::sync::atomic::AtomicU8>);

const SCREENSHOT_ACTIVE: u8 = 0;

const SCREENSHOT_CANCELLED: u8 = 1;

const SCREENSHOT_COMMITTED: u8 = 2;

impl ScreenshotCancellation {
    /// Cancel publication. Returns `false` only when the worker already won the
    /// final commit race, in which case the caller must await its real result.
    pub fn cancel(&self) -> bool {
        loop {
            match self.0.load(std::sync::atomic::Ordering::Acquire) {
                SCREENSHOT_CANCELLED => return true,
                SCREENSHOT_COMMITTED => return false,
                SCREENSHOT_ACTIVE => {
                    if self
                        .0
                        .compare_exchange(
                            SCREENSHOT_ACTIVE,
                            SCREENSHOT_CANCELLED,
                            std::sync::atomic::Ordering::AcqRel,
                            std::sync::atomic::Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return true;
                    }
                }
                _ => unreachable!("invalid screenshot publication state"),
            }
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire) == SCREENSHOT_CANCELLED
    }

    pub(crate) fn commit(&self) -> bool {
        self.0
            .compare_exchange(
                SCREENSHOT_ACTIVE,
                SCREENSHOT_COMMITTED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }
}

impl ScreenshotRequest {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(ScreenshotCancellation::is_cancelled)
    }
}

/// Filesystem policy for a screenshot destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotOutputPolicy {
    /// Kettle chose the path under its cache/state tree. Ancestors are created
    /// and verified as private before the owner-only PNG is staged.
    PrivateState,
    /// The caller explicitly chose the path. Its parent must already exist,
    /// but ordinary user directories are accepted; an owner-only sibling is
    /// atomically linked or renamed into the requested leaf only after encoding succeeds.
    UserSelected,
}

const MAX_LIVE_SCREENSHOT_BYTES: u64 = 256 * 1024 * 1024;

const LIVE_SCREENSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

const LIVE_SCREENSHOT_MAX_GPU_WAITS: u32 = 2;

pub(crate) fn screenshot_target_bytes(width: u32, height: u32) -> Option<usize> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    if bytes == 0 || bytes > MAX_LIVE_SCREENSHOT_BYTES {
        return None;
    }
    usize::try_from(bytes).ok()
}

pub(crate) struct ScreenshotCaptureTarget {
    texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    /// Process-wide charge for this short-lived render target. It travels with
    /// the readback job so accounting remains live until the GPU submission and
    /// PNG worker are both finished, not merely until command encoding ends.
    gpu: kettle_core::GraphicsReservation,
    staging: wgpu::Buffer,
    staging_gpu: kettle_core::GraphicsReservation,
    width: u32,
    height: u32,
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
}

pub(crate) struct PreparedScreenshot {
    staging: wgpu::Buffer,
    /// Keep the copy source and its transient reservation alive through the
    /// worker's bounded GPU wait. wgpu also retains the texture internally,
    /// but Kettle's accounting must cover the same lifetime.
    _capture_texture: wgpu::Texture,
    _capture_gpu: kettle_core::GraphicsReservation,
    /// The MAP_READ staging buffer is a second allocation, not part of the
    /// capture texture. Charge it independently for the same in-flight
    /// lifetime so the process GPU limit bounds actual screenshot resources.
    _staging_gpu: kettle_core::GraphicsReservation,
    width: u32,
    height: u32,
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
    format: wgpu::TextureFormat,
    /// Whether the captured surface holds kettle's premultiplied scene.
    /// `PostMultiplied` is the sole exception because its final presentation
    /// pass has already converted the completed scene to straight alpha. PNG
    /// stores straight alpha, so every direct-scene readback is converted.
    premultiplied: bool,
    request: ScreenshotRequest,
}

struct ScreenshotJob {
    device: wgpu::Device,
    gpu_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    gpu_fault: std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    submission: wgpu::SubmissionIndex,
    prepared: PreparedScreenshot,
}

struct ScreenshotPersistenceJob {
    request: ScreenshotRequest,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    format: wgpu::TextureFormat,
    premultiplied: bool,
}

pub(crate) const MAX_SCREENSHOT_PERSISTENCE_JOBS: usize = 2;

pub(crate) struct ScreenshotPersistencePermit(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl ScreenshotPersistencePermit {
    pub(crate) fn try_acquire(
        outstanding: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Option<Self> {
        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
        let acquired = outstanding.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |current| (current < MAX_SCREENSHOT_PERSISTENCE_JOBS).then_some(current + 1),
        );
        acquired.ok().map(|_| Self(outstanding.clone()))
    }
}

impl Drop for ScreenshotPersistencePermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

struct ScreenshotPersistenceWork {
    job: ScreenshotPersistenceJob,
    _permit: ScreenshotPersistencePermit,
}

#[derive(Clone)]
pub(crate) struct ScreenshotPersistencePool {
    sender: std::sync::mpsc::SyncSender<ScreenshotPersistenceWork>,
    pub(crate) outstanding: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

static SCREENSHOT_PERSISTENCE_POOL: std::sync::OnceLock<
    Result<ScreenshotPersistencePool, (std::io::ErrorKind, String)>,
> = std::sync::OnceLock::new();

impl ScreenshotPersistencePool {
    /// Return the one process-wide persistence pool.
    ///
    /// Renderers are per window and are replaced during GPU recovery. Owning
    /// these threads from a renderer would let every replacement leave another
    /// pair blocked in an encoder or filesystem call. The global retains one
    /// sender for the process lifetime, so there are exactly two persistence
    /// threads and one shared admission counter across all windows and renderer
    /// generations.
    pub(crate) fn shared() -> std::io::Result<Self> {
        match SCREENSHOT_PERSISTENCE_POOL.get_or_init(|| {
            Self::start_process_pool().map_err(|error| (error.kind(), error.to_string()))
        }) {
            Ok(pool) => Ok(pool.clone()),
            Err((kind, message)) => Err(std::io::Error::new(*kind, message.clone())),
        }
    }

    fn start_process_pool() -> std::io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ScreenshotPersistenceWork>(
            MAX_SCREENSHOT_PERSISTENCE_JOBS,
        );
        let receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));
        let outstanding = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for index in 0..MAX_SCREENSHOT_PERSISTENCE_JOBS {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("kettle-shot-save-{}", index + 1))
                .spawn(move || {
                    loop {
                        let job = receiver
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .recv();
                        let Ok(job) = job else {
                            break;
                        };
                        let ScreenshotPersistenceWork { job, _permit } = job;
                        let completion = job.request.completion.clone();
                        let result = finish_live_screenshot_persistence(job);
                        drop(_permit);
                        if let Some(tx) = completion {
                            let _ = tx.send(result.clone());
                        }
                        match result {
                            Ok(path) => log::info!("screenshot saved: {}", path.display()),
                            Err(error) => log::warn!("take_screenshot persistence failed: {error}"),
                        }
                    }
                })
                .map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!("could not create screenshot persistence worker: {error}"),
                    )
                })?;
        }
        Ok(Self {
            sender,
            outstanding,
        })
    }

    fn try_submit(
        &self,
        job: ScreenshotPersistenceJob,
    ) -> Result<(), ScreenshotPersistenceSubmitError> {
        let Some(permit) = ScreenshotPersistencePermit::try_acquire(&self.outstanding) else {
            return Err(ScreenshotPersistenceSubmitError::Busy(Box::new(job)));
        };
        let work = ScreenshotPersistenceWork {
            job,
            _permit: permit,
        };
        match self.sender.try_send(work) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(work)) => {
                Err(ScreenshotPersistenceSubmitError::Busy(Box::new(work.job)))
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(work)) => Err(
                ScreenshotPersistenceSubmitError::Disconnected(Box::new(work.job)),
            ),
        }
    }
}

enum ScreenshotPersistenceSubmitError {
    Busy(Box<ScreenshotPersistenceJob>),
    Disconnected(Box<ScreenshotPersistenceJob>),
}

pub(crate) struct ScreenshotWorker {
    sender: std::sync::mpsc::SyncSender<ScreenshotJob>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ScreenshotWorker {
    fn start() -> std::io::Result<Self> {
        let persistence = ScreenshotPersistencePool::shared()?;
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ScreenshotJob>(1);
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_busy = busy.clone();
        std::thread::Builder::new()
            .name("kettle-screenshot".to_string())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let completion = job.prepared.request.completion.clone();
                    let captured = finish_live_screenshot_capture(job);
                    // GPU capture admission ends as soon as the readback has
                    // moved into bounded CPU memory. Persistence has its own
                    // fixed-size pool, so a cancelled request stuck in an
                    // encoder or filesystem flush cannot retain GPU accounting
                    // or make every later capture BUSY indefinitely.
                    worker_busy.store(false, std::sync::atomic::Ordering::Release);
                    match captured {
                        Ok(job) => match persistence.try_submit(job) {
                            Ok(()) => {}
                            Err(ScreenshotPersistenceSubmitError::Busy(job)) => {
                                let error = "screenshot persistence workers are busy; retry after an earlier save finishes".to_string();
                                if let Some(tx) = job.request.completion {
                                    let _ = tx.send(Err(error.clone()));
                                }
                                log::warn!("take_screenshot persistence rejected: {error}");
                            }
                            Err(ScreenshotPersistenceSubmitError::Disconnected(job)) => {
                                let error = "screenshot persistence workers disconnected".to_string();
                                if let Some(tx) = job.request.completion {
                                    let _ = tx.send(Err(error.clone()));
                                }
                                log::warn!("take_screenshot persistence rejected: {error}");
                            }
                        },
                        Err(error) => {
                            if let Some(tx) = completion {
                                let _ = tx.send(Err(error.clone()));
                            }
                            log::warn!("take_screenshot capture failed: {error}");
                        }
                    }
                }
            })
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("could not create screenshot worker: {error}"),
                )
            })?;
        Ok(Self { sender, busy })
    }

    fn is_busy(&self) -> bool {
        self.busy.load(std::sync::atomic::Ordering::Acquire)
    }

    fn try_submit(&self, job: ScreenshotJob) -> Result<(), ScreenshotSubmitError> {
        if self
            .busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(ScreenshotSubmitError::Busy(Box::new(job)));
        }
        match self.sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(job)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(ScreenshotSubmitError::Busy(Box::new(job)))
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(job)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(ScreenshotSubmitError::Disconnected(Box::new(job)))
            }
        }
    }
}

enum ScreenshotSubmitError {
    Busy(Box<ScreenshotJob>),
    Disconnected(Box<ScreenshotJob>),
}

pub(crate) fn wait_for_screenshot_submission_with(
    mut poll: impl FnMut() -> Result<(), wgpu::PollError>,
    device_lost: impl Fn() -> bool,
    mut device_loss_detected: impl FnMut(),
    max_timeouts: u32,
    mut submission_stalled: impl FnMut(),
) -> Result<(), String> {
    let mut timeouts = 0_u32;
    loop {
        let polled = poll();
        // wgpu may deliver the device-lost callback while `poll` itself still
        // reports success, or alongside a backend-specific error. The flag is
        // the authoritative result and must wake an occluded event loop on
        // every path, not only after `Timeout`.
        if device_lost() {
            device_loss_detected();
            return Err("screenshot GPU wait ended because the device was lost".to_string());
        }
        match polled {
            Ok(()) => return Ok(()),
            Err(wgpu::PollError::Timeout) => {
                timeouts = timeouts.saturating_add(1);
                if timeouts >= max_timeouts {
                    submission_stalled();
                    return Err(format!(
                        "screenshot GPU submission did not retire after {timeouts} bounded waits; resetting the GPU device"
                    ));
                }
                log::warn!(
                    "screenshot GPU submission is still pending after {timeouts} bounded wait(s); retaining its resources until completion or device loss"
                );
            }
            Err(error) => return Err(format!("screenshot GPU wait failed: {error:?}")),
        }
    }
}

fn finish_live_screenshot_capture(job: ScreenshotJob) -> Result<ScreenshotPersistenceJob, String> {
    let ScreenshotJob {
        device,
        gpu_lost,
        gpu_fault,
        submission,
        prepared,
    } = job;
    let PreparedScreenshot {
        staging,
        _capture_texture: capture_texture,
        _capture_gpu: capture_gpu,
        _staging_gpu: staging_gpu,
        width,
        height,
        unpadded_bytes_per_row,
        padded_bytes_per_row,
        format,
        premultiplied,
        request,
    } = prepared;
    let recovery_wake = request.recovery_wake.clone();
    let buffer_slice = staging.slice(..);
    let (map_tx, map_rx) = std::sync::mpsc::sync_channel(1);
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = map_tx.send(result.map_err(|error| format!("{error:?}")));
    });
    // A finite wait protects the worker from one uninterruptible driver call,
    // not the accounting lifetime. `Timeout` explicitly means the submission
    // may still own the texture and staging buffer, so retry while retaining
    // `prepared` and its reservations. A latched device loss is the only safe
    // early retirement boundary.
    wait_for_screenshot_submission_with(
        || {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission.clone()),
                    timeout: Some(LIVE_SCREENSHOT_TIMEOUT),
                })
                .map(|_| ())
        },
        || gpu_lost.load(std::sync::atomic::Ordering::Acquire),
        || {
            if let Some(wake) = recovery_wake.as_ref() {
                wake.wake();
            }
        },
        LIVE_SCREENSHOT_MAX_GPU_WAITS,
        || {
            latch_gpu_fault(
                &gpu_lost,
                &gpu_fault,
                "submission_stalled",
                "screenshot GPU submission did not retire within 10 seconds".to_string(),
            );
            // A submission that has not retired after two five-second waits is
            // no longer a screenshot problem: the shared device is wedged.
            // Destroying it is the only safe boundary at which the in-flight
            // texture and staging reservations may be released. The UI sees
            // the latched fault and rebuilds every renderer on a fresh device.
            device.destroy();
            if let Some(wake) = recovery_wake.as_ref() {
                wake.wake();
            }
        },
    )?;
    map_rx
        .recv_timeout(std::time::Duration::from_millis(100))
        .map_err(|_| "screenshot map callback timed out".to_string())?
        .map_err(|error| format!("screenshot map failed: {error}"))?;

    let mapped = buffer_slice
        .get_mapped_range()
        .map_err(|error| format!("screenshot mapped range failed: {error:?}"))?;
    let pixel_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "screenshot pixel size overflow".to_string())?;
    let capacity = usize::try_from(pixel_bytes)
        .map_err(|_| "screenshot pixel buffer does not fit this platform".to_string())?;
    let bgra = matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );
    let mut rgba = Vec::with_capacity(capacity);
    for row in 0..height {
        let start = usize::try_from(u64::from(row) * u64::from(padded_bytes_per_row))
            .map_err(|_| "screenshot row offset overflow".to_string())?;
        let end = start
            .checked_add(unpadded_bytes_per_row as usize)
            .ok_or_else(|| "screenshot row end overflow".to_string())?;
        let row_pixels = mapped
            .get(start..end)
            .ok_or_else(|| "screenshot mapped buffer is shorter than expected".to_string())?;
        if bgra {
            for chunk in row_pixels.as_chunks::<4>().0 {
                rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
            }
        } else {
            rgba.extend_from_slice(row_pixels);
        }
    }
    drop(mapped);
    staging.unmap();
    // The GPU submission has retired and every byte now lives in `rgba`.
    // Drop both resources and both reservations before any encoder or
    // filesystem call can block. Kettle's GPU accounting therefore reflects
    // the actual device lifetime rather than the unrelated PNG lifetime.
    drop(staging);
    drop(capture_texture);
    drop(capture_gpu);
    drop(staging_gpu);

    if request.is_cancelled() {
        return Err("screenshot request was cancelled".to_string());
    }

    Ok(ScreenshotPersistenceJob {
        request,
        width,
        height,
        rgba,
        format,
        premultiplied,
    })
}

fn finish_live_screenshot_persistence(
    job: ScreenshotPersistenceJob,
) -> Result<std::path::PathBuf, String> {
    let ScreenshotPersistenceJob {
        request,
        width,
        height,
        rgba,
        format,
        premultiplied,
    } = job;
    if request.is_cancelled() {
        return Err("screenshot request was cancelled".to_string());
    }

    let (out_w, out_h, mut out_pixels) = crop_screenshot(width, height, rgba, request.crop)?;
    // A `PreMultiplied` surface holds premultiplied colour, and PNG stores
    // straight alpha. Converting after the crop touches only the pixels that
    // are actually saved.
    if premultiplied {
        unpremultiply_rgba8(&mut out_pixels, format.is_srgb());
    }
    use image::{ImageBuffer, Rgba};
    let image: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(out_w, out_h, out_pixels)
        .ok_or_else(|| "screenshot image buffer shape is invalid".to_string())?;
    // A terminal screenshot can contain the same class of transient
    // secrets a terminal session can (a password briefly echoed, an API
    // key, a private diff) that `kettle-core`'s `record.rs` already
    // protects by chmod'ing `.cast` files to `0o600`. Opening the PNG
    // ourselves with owner-only permissions (rather than letting
    // `ImageBuffer::save`'s internal `File::create` pick up the process
    // umask, typically `0o644` on a `022` umask) closes that gap for the
    // render crate's screenshot path too.
    if request.is_cancelled() {
        return Err("screenshot request was cancelled".to_string());
    }
    let file = create_screenshot_file(&request.out_path, request.output_policy)
        .map_err(|error| format!("screenshot output file could not be opened: {error}"))?;
    let cancellation = request.cancellation.clone();
    persist_screenshot_file_if(
        file,
        |writer| {
            image
                .write_to(writer, image::ImageFormat::Png)
                .map_err(|error| format!("PNG save failed: {error}"))
        },
        || {
            cancellation
                .as_ref()
                .is_none_or(ScreenshotCancellation::commit)
        },
    )?;
    Ok(request.out_path)
}

#[derive(Debug)]
pub(crate) struct CreatedScreenshotFile {
    file: Option<kettle_state::StagedUserSelectedFile>,
}

impl CreatedScreenshotFile {
    fn file(&self) -> &std::fs::File {
        self.file
            .as_ref()
            .expect("created screenshot file is present")
    }

    fn file_mut(&mut self) -> &mut std::fs::File {
        self.file
            .as_mut()
            .expect("created screenshot file is present")
    }

    fn sync_for_publish(&self) -> std::io::Result<()> {
        self.file
            .as_ref()
            .expect("created screenshot file is present")
            .sync_for_publish()
    }

    fn publish_synced(mut self) -> Result<(), kettle_state::StagedFilePublishError> {
        self.file
            .take()
            .expect("created screenshot file is present")
            .publish_synced()
    }

    fn discard(mut self) -> std::io::Result<()> {
        self.file
            .take()
            .expect("created screenshot file is present")
            .discard()
    }
}

impl std::ops::Deref for CreatedScreenshotFile {
    type Target = std::fs::File;

    fn deref(&self) -> &Self::Target {
        self.file()
    }
}

impl std::io::Write for CreatedScreenshotFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(self.file_mut(), buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.file_mut())
    }
}

impl std::io::Seek for CreatedScreenshotFile {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(self.file_mut(), position)
    }
}

#[cfg(test)]
pub(crate) fn persist_screenshot_file(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
) -> Result<(), String> {
    persist_screenshot_file_if(file, encode, || true)
}

pub(crate) fn persist_screenshot_file_if(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    publish: impl FnOnce() -> bool,
) -> Result<(), String> {
    persist_screenshot_file_with_flush_and_publish(file, encode, std::io::Write::flush, publish)
}

#[cfg(test)]
pub(crate) fn persist_screenshot_file_with_flush(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    flush: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> std::io::Result<()>,
) -> Result<(), String> {
    persist_screenshot_file_with_flush_and_publish(file, encode, flush, || true)
}

fn persist_screenshot_file_with_flush_and_publish(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    flush: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> std::io::Result<()>,
    begin_publication: impl FnOnce() -> bool,
) -> Result<(), String> {
    persist_screenshot_file_with_steps(
        file,
        encode,
        flush,
        CreatedScreenshotFile::sync_for_publish,
        begin_publication,
    )
}

pub(crate) fn persist_screenshot_file_with_steps(
    file: CreatedScreenshotFile,
    encode: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> Result<(), String>,
    flush: impl FnOnce(&mut std::io::BufWriter<CreatedScreenshotFile>) -> std::io::Result<()>,
    sync: impl FnOnce(&CreatedScreenshotFile) -> std::io::Result<()>,
    begin_publication: impl FnOnce() -> bool,
) -> Result<(), String> {
    fn fail_file(file: CreatedScreenshotFile, error: String) -> Result<(), String> {
        match file.discard() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}; partial-file cleanup failed: {cleanup}")),
        }
    }

    fn fail(
        writer: std::io::BufWriter<CreatedScreenshotFile>,
        error: String,
    ) -> Result<(), String> {
        let (file, _) = writer.into_parts();
        fail_file(file, error)
    }

    let mut writer = std::io::BufWriter::new(file);
    if let Err(error) = encode(&mut writer) {
        return fail(writer, error);
    }
    if let Err(error) = flush(&mut writer) {
        return fail(writer, format!("PNG save failed: {error}"));
    }
    let (file, buffered) = writer.into_parts();
    match buffered {
        Ok(bytes) if bytes.is_empty() => {}
        Ok(_) => {
            return fail_file(
                file,
                "PNG save failed: flush left buffered bytes".to_string(),
            );
        }
        Err(_) => {
            return fail_file(file, "PNG save failed: writer panicked".to_string());
        }
    }
    // Encoding, buffering, and the potentially slow inode flush deliberately
    // happen while publication remains cancellable. Commit immediately before
    // the atomic no-replace filesystem operation; if the caller won the
    // timeout race, the armed file guard removes the exact staging leaf instead
    // of allowing a late PNG to contradict the response.
    if let Err(error) = sync(&file) {
        return fail_file(file, format!("PNG durability flush failed: {error}"));
    }
    if !begin_publication() {
        return fail_file(file, "screenshot request was cancelled".to_string());
    }
    file.publish_synced()
        .map_err(|error| screenshot_publication_error(&error))?;
    Ok(())
}

fn screenshot_publication_error(error: &kettle_state::StagedFilePublishError) -> String {
    screenshot_publication_error_message(
        error.destination_may_exist(),
        error.kind(),
        &error.to_string(),
    )
}

pub(crate) fn screenshot_publication_error_message(
    kettle_may_have_published: bool,
    kind: std::io::ErrorKind,
    detail: &str,
) -> String {
    if kettle_may_have_published {
        format!(
            "PNG publication completed, but cleanup or durability verification failed; Kettle may have published the destination: {detail}"
        )
    } else if kind == std::io::ErrorKind::AlreadyExists {
        format!(
            "Kettle did not publish this screenshot because the destination already exists: {detail}"
        )
    } else {
        format!("Kettle did not publish this screenshot: {detail}")
    }
}

/// Stages an owner-only PNG sibling (`0600` on Unix, a protected current-user
/// DACL on Windows), refusing any existing destination at publication.
/// Private-state outputs also reject untrusted ancestors; an explicit
/// user-selected output pins and verifies its already-existing parent.
/// Screenshot PNGs may capture private on-screen content; see the call site in
/// `finish_live_screenshot_persistence`.
pub(crate) fn create_screenshot_file(
    path: &std::path::Path,
    policy: ScreenshotOutputPolicy,
) -> std::io::Result<CreatedScreenshotFile> {
    // The final no-replace link/rename, not the earlier
    // `validate_screenshot_path` metadata probe, is the security boundary. The
    // probe runs when the request arrives; GPU readback and encoding happen
    // later. A regular file or symlink planted during that interval therefore
    // wins with `AlreadyExists` rather than being followed or replaced. PNG
    // bytes stream only into an owner-only sibling, so the requested path is
    // absent until a complete, flushed inode is atomically published.
    if matches!(policy, ScreenshotOutputPolicy::PrivateState) {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("screenshot output has no parent: {}", path.display()),
            )
        })?;
        kettle_state::create_private_dirs(parent)?;
        kettle_state::validate_trusted_directory(parent)?;
    }
    let file = kettle_state::stage_user_selected_file_new(path)?;
    Ok(CreatedScreenshotFile { file: Some(file) })
}

pub(crate) fn crop_screenshot(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    crop: Option<(f32, f32, f32, f32)>,
) -> Result<(u32, u32, Vec<u8>), String> {
    let Some((cx, cy, cw, ch)) = crop else {
        return Ok((width, height, rgba));
    };
    let cx = cx.max(0.0) as u32;
    let cy = cy.max(0.0) as u32;
    let x_end = cx.saturating_add(cw.max(1.0) as u32).min(width);
    let y_end = cy.saturating_add(ch.max(1.0) as u32).min(height);
    let cropped_w = x_end.saturating_sub(cx);
    let cropped_h = y_end.saturating_sub(cy);
    if cropped_w == 0 || cropped_h == 0 {
        return Err("screenshot crop is outside the surface".to_string());
    }
    let capacity = u64::from(cropped_w)
        .checked_mul(u64::from(cropped_h))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| "screenshot crop size overflow".to_string())?;
    let source_stride = usize::try_from(u64::from(width) * 4)
        .map_err(|_| "screenshot source stride overflow".to_string())?;
    let col_start = usize::try_from(u64::from(cx) * 4)
        .map_err(|_| "screenshot crop column overflow".to_string())?;
    let col_end = usize::try_from(u64::from(x_end) * 4)
        .map_err(|_| "screenshot crop column overflow".to_string())?;
    let mut cropped = Vec::with_capacity(capacity);
    for y in cy..y_end {
        let row_start = usize::try_from(u64::from(y) * source_stride as u64)
            .map_err(|_| "screenshot crop row overflow".to_string())?;
        let row_end = row_start
            .checked_add(source_stride)
            .ok_or_else(|| "screenshot source row overflow".to_string())?;
        let row = rgba
            .get(row_start..row_end)
            .ok_or_else(|| "screenshot source buffer is shorter than expected".to_string())?;
        cropped.extend_from_slice(
            row.get(col_start..col_end)
                .ok_or_else(|| "screenshot crop columns are outside the source".to_string())?,
        );
    }
    Ok((cropped_w, cropped_h, cropped))
}

impl Renderer {
    /// Queue a screenshot request for the next frame. At most one capture may
    /// be pending or in flight; callers receive the original request back when
    /// the bounded worker is busy so they can report an explicit error.
    pub fn set_pending_screenshot(
        &mut self,
        req: ScreenshotRequest,
    ) -> Result<(), ScreenshotRequest> {
        if self
            .pending_screenshot
            .as_ref()
            .is_some_and(ScreenshotRequest::is_cancelled)
        {
            let cancelled = self
                .pending_screenshot
                .take()
                .expect("cancelled pending screenshot checked above");
            Self::complete_screenshot_error(cancelled, "screenshot request was cancelled".into());
        }
        if self.pending_screenshot.is_some()
            || self
                .screenshot_worker
                .as_ref()
                .is_some_and(ScreenshotWorker::is_busy)
        {
            return Err(req);
        }
        self.pending_screenshot = Some(req);
        Ok(())
    }

    /// Whether the next frame owes an explicit live-scene capture.
    ///
    /// The app uses this to distinguish ordinary background-window paints,
    /// which stay quiescent, from a requested screenshot that must render one
    /// frame even when the compositor reports the visible window as occluded.
    pub fn has_pending_screenshot(&self) -> bool {
        self.pending_screenshot
            .as_ref()
            .is_some_and(|request| !request.is_cancelled())
    }

    /// Peek and clear a queued request. Primarily retained for focused tests
    /// and callers that need to cancel before the next frame is encoded.
    pub fn take_pending_screenshot(&mut self) -> Option<ScreenshotRequest> {
        self.pending_screenshot.take()
    }

    pub(crate) fn submit_offscreen_screenshot(
        &mut self,
        encoder: wgpu::CommandEncoder,
        prepared: Option<Result<PreparedScreenshot, (ScreenshotRequest, String)>>,
    ) {
        let Some(prepared) = prepared else {
            return;
        };
        let submission = self.gpu.queue.submit(std::iter::once(encoder.finish()));
        self.dispatch_prepared_screenshot(Some(prepared), submission);
    }

    pub(crate) fn dispatch_prepared_screenshot(
        &mut self,
        prepared: Option<Result<PreparedScreenshot, (ScreenshotRequest, String)>>,
        submission: wgpu::SubmissionIndex,
    ) {
        let Some(prepared) = prepared else {
            return;
        };
        match prepared {
            Ok(prepared) => {
                let job = ScreenshotJob {
                    device: self.gpu.device.clone(),
                    gpu_lost: self.gpu.gpu_lost.clone(),
                    gpu_fault: self.gpu.gpu_fault.clone(),
                    submission,
                    prepared,
                };
                self.submit_screenshot_job(job);
            }
            Err((request, error)) => Self::complete_screenshot_error(request, error),
        }
    }

    pub(crate) fn create_screenshot_target(
        &self,
        size: [u32; 2],
    ) -> Result<ScreenshotCaptureTarget, String> {
        let bytes = screenshot_target_bytes(size[0], size[1]).ok_or_else(|| {
            "screenshot target size overflow or exceeds the 256 MiB capture limit".to_string()
        })?;
        let gpu = self
            .graphics_budget
            .reserve_transient_gpu(bytes)
            .ok_or_else(|| {
                format!(
                    "GPU graphics budget exhausted while creating a {bytes}-byte screenshot target"
                )
            })?;
        let unpadded_bytes_per_row = size[0]
            .checked_mul(4)
            .ok_or_else(|| "screenshot row size overflow".to_string())?;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(align - 1)
            .map(|row| row / align * align)
            .ok_or_else(|| "screenshot aligned row size overflow".to_string())?;
        let buffer_size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(size[1]))
            .ok_or_else(|| "screenshot readback size overflow".to_string())?;
        if buffer_size > MAX_LIVE_SCREENSHOT_BYTES {
            return Err(format!(
                "screenshot readback requires {buffer_size} bytes; limit is \
                 {MAX_LIVE_SCREENSHOT_BYTES} bytes"
            ));
        }
        let staging_bytes = usize::try_from(buffer_size)
            .map_err(|_| "screenshot staging buffer does not fit this platform".to_string())?;
        // Reserve every allocation before encoding any commands. If the second
        // reservation fails, `gpu` drops while no command buffer can yet refer
        // to the capture texture; accounting can therefore never end before a
        // submitted use of the resource retires.
        let staging_gpu = self
            .graphics_budget
            .reserve_transient_gpu(staging_bytes)
            .ok_or_else(|| {
                format!(
                    "GPU graphics budget exhausted while creating a {staging_bytes}-byte screenshot staging buffer"
                )
            })?;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-screenshot-scene"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-screenshot-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(ScreenshotCaptureTarget {
            texture,
            view,
            gpu,
            staging,
            staging_gpu,
            width: size[0],
            height: size[1],
            unpadded_bytes_per_row,
            padded_bytes_per_row,
        })
    }

    pub(crate) fn prepare_texture_screenshot(
        &self,
        capture: ScreenshotCaptureTarget,
        request: ScreenshotRequest,
        encoder: &mut wgpu::CommandEncoder,
        premultiplied: bool,
    ) -> PreparedScreenshot {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &capture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &capture.staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(capture.padded_bytes_per_row),
                    rows_per_image: Some(capture.height),
                },
            },
            wgpu::Extent3d {
                width: capture.width,
                height: capture.height,
                depth_or_array_layers: 1,
            },
        );
        let format = capture.texture.format();
        PreparedScreenshot {
            staging: capture.staging,
            _capture_texture: capture.texture,
            _capture_gpu: capture.gpu,
            _staging_gpu: capture.staging_gpu,
            width: capture.width,
            height: capture.height,
            unpadded_bytes_per_row: capture.unpadded_bytes_per_row,
            padded_bytes_per_row: capture.padded_bytes_per_row,
            format,
            premultiplied,
            request,
        }
    }

    pub(crate) fn complete_screenshot_error(request: ScreenshotRequest, error: String) {
        if let Some(tx) = request.completion {
            let _ = tx.send(Err(error.clone()));
        }
        log::warn!("take_screenshot capture failed: {error}");
    }

    fn submit_screenshot_job(&mut self, mut job: ScreenshotJob) {
        for attempt in 0..2 {
            if self.screenshot_worker.is_none() {
                match ScreenshotWorker::start() {
                    Ok(worker) => self.screenshot_worker = Some(worker),
                    Err(error) => {
                        Self::complete_screenshot_error(
                            job.prepared.request,
                            format!("screenshot worker unavailable: {error}"),
                        );
                        return;
                    }
                }
            }
            let Some(worker) = self.screenshot_worker.as_ref() else {
                unreachable!("screenshot worker was initialized above");
            };
            match worker.try_submit(job) {
                Ok(()) => return,
                Err(ScreenshotSubmitError::Busy(returned)) => {
                    Self::complete_screenshot_error(
                        returned.prepared.request,
                        "another live screenshot is already in progress".to_string(),
                    );
                    return;
                }
                Err(ScreenshotSubmitError::Disconnected(returned)) => {
                    self.screenshot_worker = None;
                    job = *returned;
                    if attempt == 1 {
                        Self::complete_screenshot_error(
                            job.prepared.request,
                            "screenshot worker disconnected".to_string(),
                        );
                        return;
                    }
                }
            }
        }
    }
}

/// Undo premultiplication on an 8-bit RGBA/BGRA readback, in the space the GPU
/// applied it in.
///
/// PNG stores straight (non-premultiplied) alpha, so a capture taken off a
/// premultiplied target has to be converted before it is saved.
///
/// `srgb_encoded` selects the space, and it is not cosmetic. On an sRGB
/// attachment the hardware decodes to linear before blending and re-encodes on
/// write, so the stored texel is an sRGB encoding of a *linear* premultiplied
/// value and the inverse has to decode, divide, and re-encode. On a plain
/// `Unorm` attachment there is no decode, the multiply happened on the stored
/// values themselves, and dividing the bytes directly is exactly right.
/// Applying either reciprocal in the other's space leaves every translucent
/// capture visibly off.
///
/// The alpha channel sits at index 3 in both RGBA and BGRA, and the operation
/// is per-channel, so this runs correctly before or after a BGRA swizzle.
///
/// A fully transparent texel carries no colour to recover, so it stays zeroed;
/// a fully opaque one is already straight and is left untouched.
pub(crate) fn unpremultiply_rgba8(pixels: &mut [u8], srgb_encoded: bool) {
    for texel in pixels.as_chunks_mut::<4>().0 {
        match texel[3] {
            0 => texel[..3].fill(0),
            u8::MAX => {}
            a => {
                let a = a as f64 / 255.0;
                for channel in &mut texel[..3] {
                    *channel = if srgb_encoded {
                        srgb_encode(srgb(*channel) / a)
                    } else {
                        (*channel as f64 / a).round().clamp(0.0, 255.0) as u8
                    };
                }
            }
        }
    }
}
