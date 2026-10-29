use super::{Arc, Config, Renderer, Result, bg_image};

/// The decoded background-image, animated. A still image is one frame; an
/// animated GIF / APNG / animated WebP is many. `frames.is_empty()` encodes a
/// failed decode, which drives the retry throttle.
pub(crate) struct BgImageAnim {
    /// The configured path this was decoded from (cache key part 1).
    pub(crate) path: String,
    /// The blur radius this was decoded with (cache key part 2).
    pub(crate) blur: u32,
    /// Arc-shared pixels keyed by identity in imgpipe's texture cache.
    /// Repeated displays can reuse the uploaded texture.
    pub(crate) frames: Vec<kettle_core::ImageData>,
    /// Whether every source texel in each parallel frame has alpha 255.
    /// Computed once when the decoded result enters the cache, not by scanning
    /// a 4K wallpaper again on every redraw.
    pub(crate) opaque_frames: Vec<bool>,
    /// Per-frame dwell time (ms), parallel to `frames`.
    pub(crate) gaps: Vec<u32>,
    /// Wall-clock origin for the playback loop (`bg_current_frame`).
    pub(crate) started: std::time::Instant,
}

/// One background-image decode request: the configured path plus the
/// resolved blur radius (0 when `background-blur` is off). Pure CPU input —
/// no GPU handles — so it's trivially `Send` across the worker thread
/// boundary, unlike `ScreenshotJob`.
pub(crate) struct BgImageJob {
    pub(crate) path: String,
    pub(crate) blur_radius: u32,
}

/// The decoded frames for a finished `BgImageJob`. Carries the request's key
/// back alongside the result so the render thread can tell a fresh result
/// from a stale one (the config may have moved on to a different image/blur
/// while this job was still decoding).
pub(crate) struct BgImageResult {
    pub(crate) path: String,
    pub(crate) blur_radius: u32,
    /// Empty when the decode failed (bad path, unsupported format, decode
    /// error), so `apply_bg_image_worker_result` caches the failed key and
    /// throttles the next retry.
    pub(crate) frames: Vec<bg_image::BgFrame>,
}

/// Lazy, single-consumer background-image decode worker. Mirrors
/// `ScreenshotWorker`'s shape (a `busy` flag guarding a capacity-1 job
/// channel) but adds a result channel: unlike a screenshot save, a finished
/// decode has to feed data back into `render_frame` rather than just log a
/// side effect.
pub(crate) struct BgImageWorker {
    pub(crate) sender: std::sync::mpsc::SyncSender<BgImageJob>,
    pub(crate) receiver: std::sync::mpsc::Receiver<BgImageResult>,
    pub(crate) busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) enum BgImageSubmitError {
    /// A different job is still decoding on the worker thread. The caller
    /// should just wait — `apply_bg_image_worker_result` will pick up the
    /// in-flight job's result once it lands, and `request_bg_image_reload`
    /// will resubmit for the (still-current) desired key on a later frame.
    Busy,
    /// The worker thread exited (e.g. panicked mid-decode). The caller
    /// drops the worker so the next reload attempt spawns a fresh one.
    Disconnected,
}

impl BgImageWorker {
    pub(crate) fn start(redraw: Option<Arc<dyn Fn() + Send + Sync>>) -> std::io::Result<Self> {
        let (sender, job_rx) = std::sync::mpsc::sync_channel::<BgImageJob>(1);
        let (result_tx, receiver) = std::sync::mpsc::channel::<BgImageResult>();
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_busy = busy.clone();
        std::thread::Builder::new()
            .name("kettle-bg-image".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let frames =
                        bg_image::decode_bg_image_frames_with_blur(&job.path, job.blur_radius)
                            .unwrap_or_default();
                    let delivered = result_tx.send(BgImageResult {
                        path: job.path,
                        blur_radius: job.blur_radius,
                        frames,
                    });
                    worker_busy.store(false, std::sync::atomic::Ordering::Release);
                    if delivered.is_err() {
                        // The Renderer (and its `BgImageWorker`) was dropped —
                        // no one is left to hand results to.
                        break;
                    }
                    if let Some(redraw) = redraw.as_ref() {
                        redraw();
                    }
                }
            })
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("could not create background-image decode worker: {error}"),
                )
            })?;
        Ok(Self {
            sender,
            receiver,
            busy,
        })
    }

    pub(crate) fn try_submit(&self, job: BgImageJob) -> Result<(), BgImageSubmitError> {
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
            return Err(BgImageSubmitError::Busy);
        }
        match self.sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(BgImageSubmitError::Busy)
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                Err(BgImageSubmitError::Disconnected)
            }
        }
    }

    /// Non-blocking: `None` when no decode has finished since the last poll.
    pub(crate) fn try_recv(&self) -> Option<BgImageResult> {
        self.receiver.try_recv().ok()
    }
}

/// The starfield shader's clock. `background-animation = off` freezes it on
/// its first frame, as the image path shows frame 0. Otherwise it runs, and a
/// repaint while animation is paused for focus shows the time-correct frame.
pub(crate) fn starfield_time(
    animation: kettle_config::BackgroundAnimation,
    elapsed: std::time::Duration,
) -> f32 {
    if animation == kettle_config::BackgroundAnimation::Off {
        0.0
    } else {
        elapsed.as_secs_f32()
    }
}

impl Renderer {
    /// Repaint cap for the procedural starfield (`background-type = starfield`).
    /// The drift is slow, so a low rate keeps idle CPU near today's level while
    /// the steps stay imperceptible; the shader's `time` is continuous either
    /// way. A synthetic frame index (`elapsed / 100 ms`) lets the starfield
    /// reuse the GIF's edge-trigger + wake machinery unchanged.
    const STARFIELD_FPS: u64 = 10;

    /// Whether the selected background should advance proactively. Focus gates
    /// `when-focused`; the event loop separately suppresses hidden-window wakes.
    /// Other repaints still select the time-correct frame.
    pub fn background_is_animating(&self, cfg: &Config, window_focused: bool) -> bool {
        use kettle_config::BackgroundType;
        // Shared focus/Off gate.
        let enabled = match cfg.background_animation {
            kettle_config::BackgroundAnimation::Off => false,
            kettle_config::BackgroundAnimation::Always => true,
            kettle_config::BackgroundAnimation::WhenFocused => window_focused,
        };
        if !enabled {
            return false;
        }
        match cfg.background_type {
            // Procedural — always advancing while enabled (no frame cache).
            BackgroundType::Starfield => true,
            // Decoded — only a genuinely multi-frame image animates.
            BackgroundType::Image => self
                .bg_image_cache
                .as_ref()
                .is_some_and(|c| c.frames.len() > 1),
            BackgroundType::Solid | BackgroundType::Transparent => false,
        }
    }

    /// Synthetic frame period (ms) for the starfield's fps cap.
    fn starfield_frame_ms() -> u128 {
        (1000 / Self::STARFIELD_FPS) as u128
    }

    /// Milliseconds until the animated background's displayed frame next
    /// changes. The event loop uses this as the bg-animation wake interval.
    /// Waking at the actual frame boundary caps repaints at the GIF's own fps,
    /// where a fixed 30 fps tick would repaint the SAME frame ~22×/s for a
    /// typical 8 fps GIF (wasted full-surface `present()`s). `None` when the
    /// background isn't animating. Floored at 16 ms so a degenerate fast GIF
    /// can't drive the loop past ~60 fps.
    pub fn bg_anim_interval_ms(&self, cfg: &Config, window_focused: bool) -> Option<u64> {
        if !self.background_is_animating(cfg, window_focused) {
            return None;
        }
        if matches!(
            cfg.background_type,
            kettle_config::BackgroundType::Starfield
        ) {
            // Wake at the next fps-cap boundary.
            let frame = Self::starfield_frame_ms();
            let into = self.starfield_started.elapsed().as_millis() % frame;
            return Some(((frame - into) as u64).max(16));
        }
        let c = self.bg_image_cache.as_ref()?;
        bg_image::bg_next_frame_ms(&c.gaps, c.started.elapsed().as_millis())
    }

    /// The animated background's currently-displayed frame index, or `None`
    /// when it isn't animating. The event loop compares this against the
    /// last-painted index and requests a redraw ONLY when it changes (an
    /// edge-trigger, like the cursor blink). Calling `request_redraw` on every
    /// `about_to_wait` while the bg animates would make winit redraw
    /// continuously (vsync-bound) instead of at the GIF's fps, which keeps
    /// animated-idle CPU high.
    pub fn bg_current_frame_index(&self, cfg: &Config, window_focused: bool) -> Option<usize> {
        if !self.background_is_animating(cfg, window_focused) {
            return None;
        }
        if matches!(
            cfg.background_type,
            kettle_config::BackgroundType::Starfield
        ) {
            // Quantize continuous time to the fps cap so the edge-trigger fires
            // once per displayed step.
            let frame = Self::starfield_frame_ms();
            return Some((self.starfield_started.elapsed().as_millis() / frame) as usize);
        }
        let c = self.bg_image_cache.as_ref()?;
        if c.frames.len() <= 1 {
            return None;
        }
        Some(bg_image::bg_current_frame(
            &c.gaps,
            c.started.elapsed().as_millis(),
        ))
    }

    /// Install the owning window's redraw callback before its first frame.
    pub fn ensure_background_redraw(&mut self, redraw: impl Fn() + Send + Sync + 'static) {
        if self.bg_image_redraw.is_none() {
            self.bg_image_redraw = Some(Arc::new(redraw));
        }
    }

    /// (Re)submit a background-image decode to `bg_image_worker`, starting
    /// the worker thread on first use. A no-op when a job for this exact
    /// `(path, blur_radius)` key is already in flight, so a `need_reload ==
    /// true` frame that fires every frame while the decode is still running
    /// (a large/animated wallpaper can take well over one frame at 60 fps)
    /// doesn't flood the worker's capacity-1 job channel with redundant work.
    pub(crate) fn request_bg_image_reload(&mut self, path: &str, blur_radius: u32) {
        if self
            .bg_image_pending
            .as_ref()
            .is_some_and(|(p, b)| p == path && *b == blur_radius)
        {
            return;
        }
        if self.bg_image_worker.is_none() {
            match BgImageWorker::start(self.bg_image_redraw.clone()) {
                Ok(worker) => self.bg_image_worker = Some(worker),
                Err(error) => {
                    log::warn!("could not start background-image decode worker: {error}");
                    return;
                }
            }
        }
        let Some(worker) = self.bg_image_worker.as_ref() else {
            return;
        };
        match worker.try_submit(BgImageJob {
            path: path.to_string(),
            blur_radius,
        }) {
            Ok(()) => self.bg_image_pending = Some((path.to_string(), blur_radius)),
            Err(BgImageSubmitError::Busy) => {
                // A different job is still decoding (shouldn't normally
                // happen given the key check above, but config could churn
                // faster than the worker drains) — this frame keeps showing
                // the previous wallpaper (or none); retried next frame.
            }
            Err(BgImageSubmitError::Disconnected) => {
                // Worker thread exited — drop it so the NEXT reload attempt
                // spawns a fresh one instead of submitting into a dead
                // channel every frame.
                self.bg_image_worker = None;
            }
        }
    }

    /// Drain finished decodes, installing only the pending job whose key also
    /// matches the current configuration. Clear a completed pending job even
    /// when its result is obsolete, so the same key can be requested again.
    pub(crate) fn apply_bg_image_worker_result(&mut self, path: &str, blur_radius: u32) {
        let Some(worker) = self.bg_image_worker.as_ref() else {
            return;
        };
        while let Some(result) = worker.try_recv() {
            let is_pending = self
                .bg_image_pending
                .as_ref()
                .is_some_and(|(p, b)| *p == result.path && *b == result.blur_radius);
            if !is_pending {
                continue;
            }
            self.bg_image_pending = None;
            if result.path != path || result.blur_radius != blur_radius {
                continue;
            }
            let mut frames = Vec::with_capacity(result.frames.len());
            let mut opaque_frames = Vec::with_capacity(result.frames.len());
            let mut gaps = Vec::with_capacity(result.frames.len());
            for frame in result.frames {
                let opaque = frame
                    .image
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255);
                if let Some(image) = kettle_core::ImageData::new_with_budget(
                    frame.image.width,
                    frame.image.height,
                    frame.image.rgba,
                    &self.graphics_budget,
                ) {
                    frames.push(image);
                    opaque_frames.push(opaque);
                    gaps.push(frame.gap_ms);
                }
            }
            // On failure, throttle the next retry; on success, clear it so
            // the loaded wallpaper never re-decodes.
            self.bg_image_retry_at = if frames.is_empty() {
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3))
            } else {
                None
            };
            self.bg_image_cache = Some(BgImageAnim {
                path: result.path,
                blur: result.blur_radius,
                frames,
                opaque_frames,
                gaps,
                started: std::time::Instant::now(),
            });
        }
    }
}
