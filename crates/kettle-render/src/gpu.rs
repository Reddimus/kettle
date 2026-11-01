use super::{
    Arc, Attrs, Cache, Config, DisplayHandle, GlyphPipeline, HandleError, HasDisplayHandle,
    HasWindowHandle, MinimumContrastCache, OutlinePipeline, Renderer, Result, SEARCH_SEGMENTS,
    ScreenshotRecoveryWake, ScreenshotRequest, Shaping, SharedPipelines, StartupFonts, SwashCache,
    TextAtlas, TextBuffer, TextRenderer, Viewport, Weight, Wrap, anyhow, card_scene, chrome,
    clamp_font_size, confirm, desired_alpha_mode, live_surface_dimensions, metrics_for, present,
    rounded_surface, starfield, upload,
};

/// The process-wide GPU objects shared by every window's
/// Renderer. wgpu's Instance/Adapter/Device/Queue handles are internally
/// ref-counted — `Clone` is a refcount bump, and one device happily serves N
/// surfaces. Window 1 creates this inside `Renderer::new`; windows 2..N reuse
/// it via the synchronous `Renderer::new_with_gpu` (no adapter/device
/// request, no block_on, no GPU-init watchdog needed).
#[derive(Clone)]
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Set `true` when wgpu reports a fatal uncaptured error or a device loss
    /// (a GPU driver TDR/reset, VRAM exhaustion, or internal backend fault).
    /// Validation errors are logged but deliberately excluded. The handlers
    /// installed in `install_gpu_error_handlers` set this flag instead of
    /// letting wgpu's default handler panic, which under the release profile's
    /// `panic = "abort"` would kill kettle on a GPU reset with no crash log.
    /// The App checks this (a refcount-shared `Arc`, so every window's clone
    /// sees it) to stop rendering on a dead device and surface a "GPU device
    /// lost" state rather than spin or crash. Reset by rebuilding the renderer
    /// on a fresh context.
    pub gpu_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// First fatal wgpu error for this device. Error callbacks only latch this
    /// bounded in-memory value; the UI thread owns durable diagnostics so a
    /// driver callback never blocks on filesystem I/O.
    pub(crate) gpu_fault: std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    /// Application event-loop wake shared with driver callbacks. The context
    /// is created before the UI owns a live proxy, so the App installs this
    /// immediately after renderer construction.
    recovery_wake: std::sync::Arc<std::sync::Mutex<Option<ScreenshotRecoveryWake>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuFault {
    pub kind: String,
    pub message: String,
}

/// Result of attempting to paint one live surface frame.
///
/// A successful Rust return does not necessarily mean pixels reached the
/// compositor: wgpu reports timeout, occlusion, and swapchain-loss conditions
/// as normal acquisition outcomes.  Callers must only consume terminal/UI
/// damage after [`Presented`](Self::Presented).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum FrameOutcome {
    /// Queue submission completed and the surface texture was presented.
    Presented,
    /// Nothing was presented, but the configured surface remains usable.
    /// Retain damage and retry through the caller's deadline-driven backoff.
    RetryLater,
    /// Nothing was presented because the window is not currently visible.
    /// Retain damage and wait for an un-occlusion/visibility event.
    Occluded,
    /// The surface was lost and must be recreated through
    /// [`Instance::create_surface`](wgpu::Instance::create_surface) before
    /// another frame is attempted. The shared device can remain healthy.
    SurfaceLost,
}

impl GpuContext {
    /// Whether the GPU device was lost (driver reset / TDR) or hit a fatal error
    /// (e.g. VRAM exhaustion). Once `true`, no rendering will succeed against
    /// this context.
    pub fn is_lost(&self) -> bool {
        self.gpu_lost.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Return the first fatal error reported for this device. The value stays
    /// available throughout recovery and is reset with a new [`GpuContext`].
    pub fn fault(&self) -> Option<GpuFault> {
        self.gpu_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_recovery_wake(&self, wake: ScreenshotRecoveryWake) {
        *self
            .recovery_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
    }

    pub fn is_software(&self) -> bool {
        matches!(self.adapter.get_info().device_type, wgpu::DeviceType::Cpu)
    }

    /// Whether per-frame instances go through mapped buffers on this device
    /// (see `upload::mapped_upload_features`).
    pub fn mapped_uploads(&self) -> bool {
        upload::mapped_uploads(&self.device)
    }

    pub fn adapter_name(&self) -> String {
        self.adapter.get_info().name
    }

    pub fn adapter_ids(&self) -> (u32, u32) {
        let info = self.adapter.get_info();
        (info.vendor, info.device)
    }

    pub fn adapter_key(&self) -> GpuAdapterKey {
        GpuAdapterKey::from_info(&self.adapter.get_info())
    }
}

/// Installs wgpu's uncaptured-error and device-lost handlers so a GPU fault is
/// logged instead of hitting wgpu's default panic. Under the release
/// `panic = "abort"`, that panic kills kettle with no unwind and no log on a
/// driver TDR/reset or a VRAM allocation failure. A fatal fault sets the shared
/// `gpu_lost` flag, and the App degrades gracefully.
fn install_gpu_error_handlers(
    device: &wgpu::Device,
    gpu_lost: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    gpu_fault: &std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    recovery_wake: &std::sync::Arc<std::sync::Mutex<Option<ScreenshotRecoveryWake>>>,
) {
    let flag = gpu_lost.clone();
    let fault = gpu_fault.clone();
    let wake = recovery_wake.clone();
    // The uncaptured-error path catches errors NOT routed to an error scope.
    // wgpu delivers a real device loss through `set_device_lost_callback`
    // below, not here; this handler only sees `Validation`, `OutOfMemory`, and
    // `Internal`. A `Validation` error is a kettle code/data bug (bad
    // descriptor, over-limit dim, stale bind group) on a healthy device, so it
    // is logged loudly but does not set `gpu_lost`, which would falsely brick
    // the window with "GPU device lost". Only `OutOfMemory` (VRAM exhaustion)
    // and `Internal` are device-fatal. `Fn`; must not panic.
    device.on_uncaptured_error(std::sync::Arc::new(move |e: wgpu::Error| match e {
        wgpu::Error::Validation { .. } => {
            log::error!("wgpu validation error (a kettle bug, NOT device loss): {e}");
        }
        wgpu::Error::OutOfMemory { .. } => {
            log::error!("wgpu fatal GPU error (out of memory): {e}");
            latch_gpu_fault(&flag, &fault, "out_of_memory", e.to_string());
            wake_gpu_recovery(&wake);
        }
        wgpu::Error::Internal { .. } => {
            log::error!("wgpu fatal GPU error (internal): {e}");
            latch_gpu_fault(&flag, &fault, "internal", e.to_string());
            wake_gpu_recovery(&wake);
        }
    }));
    let flag2 = gpu_lost.clone();
    let fault2 = gpu_fault.clone();
    let wake2 = recovery_wake.clone();
    device.set_device_lost_callback(move |reason, msg| {
        // `Destroyed` fires only after our own `device.destroy()` (the wedged
        // screenshot reset, which latches its own fault first), so it needs no
        // flag here. Only an `Unknown` loss (driver TDR/reset) flags.
        if !matches!(reason, wgpu::DeviceLostReason::Destroyed) {
            log::error!("wgpu device lost ({reason:?}): {msg}");
            latch_gpu_fault(&flag2, &fault2, "device_lost", msg);
            wake_gpu_recovery(&wake2);
        }
    });
}

fn wake_gpu_recovery(
    recovery_wake: &std::sync::Arc<std::sync::Mutex<Option<ScreenshotRecoveryWake>>>,
) {
    let wake = recovery_wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(wake) = wake {
        wake.wake();
    }
}

pub(crate) fn latch_gpu_fault(
    gpu_lost: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    gpu_fault: &std::sync::Arc<std::sync::Mutex<Option<GpuFault>>>,
    kind: &str,
    message: String,
) {
    let mut fault = gpu_fault
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if fault.is_none() {
        *fault = Some(GpuFault {
            kind: kind.to_string(),
            message: bounded_gpu_message(&message),
        });
    }
    drop(fault);
    gpu_lost.store(true, std::sync::atomic::Ordering::Release);
}

fn bounded_gpu_message(message: &str) -> String {
    const MAX_CHARS: usize = 2048;
    let mut out = String::with_capacity(message.len().min(MAX_CHARS));
    for ch in message.chars().take(MAX_CHARS) {
        out.push(if ch.is_control() { ' ' } else { ch });
    }
    out
}

impl GpuContext {
    /// The live adapter's identity, in kettle's vocabulary. It feeds the
    /// settings `Active GPU: <gpu> (<kind>, <backend>)` line, so the user sees
    /// which GPU is actually in use rather than the pinned/preferred one, which
    /// only takes effect on restart.
    pub fn adapter_info(&self) -> GpuAdapterInfo {
        let i = self.adapter.get_info();
        GpuAdapterInfo {
            name: i.name,
            vendor: i.vendor,
            device: i.device,
            kind: device_kind_str(i.device_type),
            backend: backend_str(i.backend),
        }
    }
}

/// CPU-owned renderer state that must survive a surface/device rebuild.
///
/// GPU resources and retained draw caches deliberately stay out of this
/// snapshot: they are tied to the failed device and are rebuilt lazily. The
/// values here are the live per-window overrides that may have diverged from
/// [`Config`] since launch, plus a screenshot request that has not yet been
/// submitted to the worker. The window accent is not among them: the App owns
/// it, can change it while no renderer exists, and applies it to every
/// replacement renderer, so a copy here could only go stale.
#[derive(Clone, Debug)]
pub struct RendererRecoveryState {
    pub(crate) font_family: Arc<str>,
    pub(crate) font_size: f32,
    pub(crate) cell_scale_w: f32,
    pub(crate) cell_scale_h: f32,
    pub(crate) pending_screenshot: Option<ScreenshotRequest>,
}

pub(crate) fn live_device_limits(adapter_limits: wgpu::Limits) -> wgpu::Limits {
    // Start with Kettle's normal WebGPU defaults, keep the adapter's full
    // surface resolution, then clamp every remaining request to what the
    // adapter actually advertises. Virtual GLES adapters can legitimately
    // expose no compute pipeline at all (`max_compute_workgroups... == 0`).
    // Asking them for wgpu's default 65_535 workgroups fails device creation
    // even though Kettle renders entirely through graphics pipelines. This is
    // also safer than requesting every adapter maximum: the application keeps
    // its deliberately small resource envelope except where presentation
    // resolution requires more.
    wgpu::Limits {
        max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
        ..Default::default()
    }
    .or_worse_values_from(&adapter_limits)
}

impl Renderer {
    /// Compatibility constructor for embedders that only provide a window.
    ///
    /// Winit applications should prefer [`Renderer::new_with_display_handle`]
    /// and pass `ActiveEventLoop::owned_display_handle()`. The owned event-loop
    /// handle lets wgpu initialize the Wayland GLES presentation path without
    /// retaining the window for the lifetime of the shared GPU context.
    pub async fn new<W>(
        window: Arc<W>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        Self::new_with_escalation(
            window,
            width,
            height,
            scale,
            cfg,
            AdapterEscalation::Preferred,
            None,
        )
        .await
    }

    /// Create a renderer while retaining an owned display connection separately
    /// from the surface window.
    ///
    /// For winit, `display_handle` should be
    /// `ActiveEventLoop::owned_display_handle()`. It is cheap to clone and,
    /// unlike an `Arc<Window>`, does not keep a closed OS window alive.
    ///
    /// `fonts`, when loaded for the same font and scale, are taken over
    /// instead of loading the system fonts again.
    pub async fn new_with_display_handle<W, D>(
        window: Arc<W>,
        display_handle: D,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        Self::new_with_escalation_inner(
            window,
            Some(OwnedGpuDisplayHandle::new(display_handle)),
            width,
            height,
            scale,
            cfg,
            AdapterSelection::new(AdapterEscalation::Preferred, None),
            fonts,
        )
        .await
    }

    pub async fn new_with_escalation<W>(
        window: Arc<W>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        escalation: AdapterEscalation,
        avoid: Option<GpuAdapterKey>,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        Self::new_with_escalation_inner(
            window,
            None,
            width,
            height,
            scale,
            cfg,
            AdapterSelection::new(escalation, avoid),
            None,
        )
        .await
    }

    /// Recovery-capable variant of [`Renderer::new_with_display_handle`].
    pub async fn new_with_escalation_and_display_handle<W, D>(
        window: Arc<W>,
        display_handle: D,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        selection: AdapterSelection,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        Self::new_with_escalation_inner(
            window,
            Some(OwnedGpuDisplayHandle::new(display_handle)),
            width,
            height,
            scale,
            cfg,
            selection,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn new_with_escalation_inner<W>(
        window: Arc<W>,
        display_handle: Option<OwnedGpuDisplayHandle>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        selection: AdapterSelection,
        fonts: Option<StartupFonts>,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        // Default, unpinned Auto startup probes one native backend at a time.
        // On Windows a successful DX12 request therefore never loads the
        // Vulkan ICD. Pins and explicit low/high policy need a cross-backend
        // view, while recovery deliberately inspects alternate adapters.
        // Startup is measured, not guessed. This init blocks the event-loop
        // thread and the window stays hidden until the first paint, so every
        // millisecond here is time the user spends looking at nothing --
        // and a comparator benchmark can only report the total. Splitting it
        // three ways says WHICH part to attack.
        let t_start = std::time::Instant::now();
        let (instance, surface, adapters) = resolve_window_adapter(
            window,
            display_handle.as_ref(),
            cfg,
            selection.escalation,
            selection.avoid,
            "Renderer::new",
        )
        .await?;
        let adapter_ms = t_start.elapsed().as_secs_f64() * 1000.0;
        let t_device = std::time::Instant::now();
        // wgpu's default requested limit is 8192 even when the adapter can
        // present a larger surface. Request the adapter's real 2D limit up
        // front so later high-DPI or multi-display resizes can configure the
        // swapchain at the window's actual physical dimensions.
        let (adapter, (device, queue)) = try_adapter_candidates(adapters, |adapter| async move {
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("kettle-device"),
                    required_features: upload::mapped_upload_features(&adapter),
                    required_limits: live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow!("failed to create device: {e:?}"))
        })
        .await?;
        // Turn a GPU driver reset (TDR) / VRAM exhaustion into a logged,
        // observable event instead of wgpu's default panic, which `panic=abort`
        // makes a hard crash. Installed once on the shared device.
        let gpu_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gpu_fault = std::sync::Arc::new(std::sync::Mutex::new(None));
        let recovery_wake = std::sync::Arc::new(std::sync::Mutex::new(None));
        install_gpu_error_handlers(&device, &gpu_lost, &gpu_fault, &recovery_wake);
        let gpu = GpuContext {
            instance,
            adapter,
            device,
            queue,
            gpu_lost,
            gpu_fault,
            recovery_wake,
        };
        let device_ms = t_device.elapsed().as_secs_f64() * 1000.0;
        let t_rest = std::time::Instant::now();
        let built = Self::with_gpu_and_surface(gpu, surface, width, height, scale, cfg, fonts);
        // This span covers everything after device creation, INCLUDING the
        // font-system time logged separately, so do not add the two together.
        log::info!(
            "renderer init: adapter {adapter_ms:.1}ms, device {device_ms:.1}ms,              surface+fonts+pipelines {:.1}ms (font init logged separately is              part of it), total {:.1}ms",
            t_rest.elapsed().as_secs_f64() * 1000.0,
            t_start.elapsed().as_secs_f64() * 1000.0
        );
        built
    }

    /// Synchronous constructor for windows 2..N. Reuses the shared
    /// [`GpuContext`] instead of requesting an adapter/device, so it never
    /// blocks the event loop (the ~1.5s async init and its hung-driver
    /// watchdog are a window-1-only cost). Fails cleanly if the shared adapter
    /// can't present to the new window's surface (e.g. a window on a display
    /// driven by a different GPU); the caller then keeps the tab where it was.
    pub fn new_with_gpu<W>(
        gpu: &GpuContext,
        window: Arc<W>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
    ) -> Result<Renderer>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        let surface = gpu.instance.create_surface(window)?;
        if !gpu.adapter.is_surface_supported(&surface) {
            return Err(anyhow!(
                "the shared GPU adapter cannot present to the new window's surface"
            ));
        }
        Self::with_gpu_and_surface(gpu.clone(), surface, width, height, scale, cfg, fonts)
    }

    /// Shared constructor tail: everything after a surface + GPU exist
    /// (format/alpha selection, surface configure, font system, pipelines).
    fn with_gpu_and_surface(
        gpu: GpuContext,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
    ) -> Result<Renderer> {
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        Self::with_gpu(
            gpu,
            Some(surface),
            format,
            caps.alpha_modes,
            width,
            height,
            scale,
            cfg,
            fonts,
        )
    }

    /// A renderer with no window, drawing only into the offscreen capture
    /// target, so tests can render real panes through the live frame path.
    /// `None` when the host has no GPU adapter.
    #[cfg(test)]
    pub(crate) fn headless_for_tests(
        cfg: &Config,
        width: u32,
        height: u32,
    ) -> Result<Option<Renderer>> {
        Self::headless_for_tests_with(cfg, width, height, 1.0, false)
    }

    /// [`Self::headless_for_tests`] at `scale`. With `translucent_surface`
    /// the pretend surface also offers PostMultiplied, as a macOS window does,
    /// so a translucent config takes the translucent path.
    #[cfg(test)]
    pub(crate) fn headless_for_tests_with(
        cfg: &Config,
        width: u32,
        height: u32,
        scale: f32,
        translucent_surface: bool,
    ) -> Result<Option<Renderer>> {
        Self::headless_for_tests_with_mapping(cfg, width, height, scale, translucent_surface, true)
    }

    /// [`Self::headless_for_tests_with`], and with `allow_mapping` false the
    /// queue upload path on the same adapter the live upload path uses.
    #[cfg(test)]
    pub(crate) fn headless_for_tests_with_mapping(
        cfg: &Config,
        width: u32,
        height: u32,
        scale: f32,
        translucent_surface: bool,
        allow_mapping: bool,
    ) -> Result<Option<Renderer>> {
        pollster::block_on(async {
            let Ok((instance, adapter)) = resolve_headless_adapter(cfg, "headless-renderer").await
            else {
                return Ok(None);
            };
            // The live device's features, so these tests take the upload
            // path a window on this host takes.
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("kettle-headless-renderer"),
                    required_features: if allow_mapping {
                        upload::mapped_upload_features(&adapter)
                    } else {
                        wgpu::Features::empty()
                    },
                    required_limits: live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow!("device: {e:?}"))?;
            let gpu = GpuContext {
                instance,
                adapter,
                device,
                queue,
                gpu_lost: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                gpu_fault: std::sync::Arc::new(std::sync::Mutex::new(None)),
                recovery_wake: std::sync::Arc::new(std::sync::Mutex::new(None)),
            };
            let mut alpha_modes = vec![wgpu::CompositeAlphaMode::Opaque];
            if translucent_surface {
                alpha_modes.push(wgpu::CompositeAlphaMode::PostMultiplied);
            }
            Self::with_gpu(
                gpu,
                None,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                alpha_modes,
                width,
                height,
                scale,
                cfg,
                None,
            )
            .map(Some)
        })
    }

    /// Shared tail of every constructor, once a GPU and (for a window) a
    /// surface exist.
    #[allow(clippy::too_many_arguments)]
    fn with_gpu(
        gpu: GpuContext,
        surface: Option<wgpu::Surface<'static>>,
        format: wgpu::TextureFormat,
        supported_alpha_modes: Vec<wgpu::CompositeAlphaMode>,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
    ) -> Result<Renderer> {
        let GpuContext { device, queue, .. } = gpu.clone();
        let alpha_mode = desired_alpha_mode(cfg, &supported_alpha_modes);
        let (width, height) =
            live_surface_dimensions(width, height, device.limits().max_texture_dimension_2d);
        let config = wgpu::SurfaceConfiguration {
            // Screenshots read the offscreen scene target, not the swapchain.
            // Keeping presentation to its sole required usage also supports
            // RDP/virtual adapters that advertise no surface COPY_SRC.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        if let Some(surface) = &surface {
            surface.configure(&device, &config);
        }

        // Fonts loaded before the window reach here already measured. Ones
        // measured for another scale or font are measured again, without
        // enumerating the system fonts a second time.
        let StartupFonts {
            mut font_system,
            text_symbol_family,
            cell: (cell_w, cell_h),
            ..
        } = match fonts {
            Some(fonts) if fonts.matches(cfg, scale) => fonts,
            Some(fonts) => fonts.remeasure(cfg, scale),
            None => StartupFonts::load(cfg, scale),
        };
        let cell_scale_w = cfg.cell_width.max(0.01);
        let cell_scale_h = cfg.cell_height.max(0.01);

        let swash = SwashCache::new();
        let cache = Cache::new(&device);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let viewport = Viewport::new(&device, &cache);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        // Clamp `cfg.font_size` to the same [5.0, 72.0] range as the runtime
        // `set_font_size` path. Without this, a user config of
        // `font-size = 200` boots the renderer with 200pt cells and hits the
        // wgpu 8192px-per-side texture limit (or floods the window with one
        // giant glyph). 5.0 is below "tiny but legible"; 72.0 is "billboard".
        let font_size = clamp_font_size(cfg.font_size);
        // Physical-pixel metrics — logical font size × DPI scale.
        let metrics = metrics_for(font_size, scale);
        let tabbar_buffer = TextBuffer::new(&mut font_system, metrics);
        let new_tab_arrow_buffer = TextBuffer::new(&mut font_system, metrics);
        let scroll_left_buffer = TextBuffer::new(&mut font_system, metrics);
        let scroll_right_buffer = TextBuffer::new(&mut font_system, metrics);
        let tab_close_buffer = TextBuffer::new(&mut font_system, metrics);
        let search_segment_buffers = (0..SEARCH_SEGMENTS)
            .map(|_| {
                let mut buffer = TextBuffer::new(&mut font_system, metrics);
                buffer.set_wrap(Wrap::None);
                buffer
            })
            .collect();
        let status_bar_buffer = TextBuffer::new(&mut font_system, metrics);
        let resize_overlay_buffer = TextBuffer::new(&mut font_system, metrics);
        let update_toast_buffers =
            std::array::from_fn(|_| TextBuffer::new(&mut font_system, metrics));
        let mut completion_header_buffer = TextBuffer::new(&mut font_system, metrics);
        completion_header_buffer.set_wrap(Wrap::None);
        let mut completion_count_buffer = TextBuffer::new(&mut font_system, metrics);
        completion_count_buffer.set_wrap(Wrap::None);
        let mut media_receipt_title_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_title_buffer.set_wrap(Wrap::None);
        let mut media_receipt_detail_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_detail_buffer.set_wrap(Wrap::None);
        let mut media_receipt_dismiss_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_dismiss_buffer.set_wrap(Wrap::None);
        media_receipt_dismiss_buffer.set_text(
            "×",
            &Attrs::new().weight(Weight::BOLD),
            Shaping::Advanced,
            None,
        );
        let mut media_receipt_badge_buffer = TextBuffer::new(&mut font_system, metrics);
        media_receipt_badge_buffer.set_wrap(Wrap::None);
        media_receipt_badge_buffer.set_text(
            "▶",
            &Attrs::new().weight(Weight::BOLD),
            Shaping::Advanced,
            None,
        );
        let mut ime_buffer = TextBuffer::new(&mut font_system, metrics);
        ime_buffer.set_wrap(Wrap::None);

        let shared = SharedPipelines::new(&device, format);
        let pane_bases = shared.quads.replace(&device);
        let live_pane_bases = shared.quads.replace(&device);
        let quads = shared.quads.blend(&device);
        let pane_outlines = OutlinePipeline::new(&device, format);
        let pane_dimming = shared.quads.blend(&device);
        let text_decorations = shared.quads.blend(&device);
        let overlay_quads = shared.quads.blend(&device);
        let menu_quads = shared.quads.blend(&device);
        let ime_quads = shared.quads.blend(&device);
        let ime_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let menu_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let rounded_surfaces = rounded_surface::RoundedSurfacePipeline::new(&device, format);
        let toast_surfaces = rounded_surface::RoundedSurfacePipeline::new(&device, format);
        let confirmation_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let confirmation_text = confirm::ConfirmText::new();
        let cursor_glyph_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let cursor_glyph_buffer = TextBuffer::new(&mut font_system, metrics);
        let graphics_budget = kettle_core::GraphicsBudget::default();
        let card_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let card_base = shared.quads.blend(&device);
        let lane_quads = shared.quads.blend(&device);
        let lane_text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let card_cursors = shared.quads.blend(&device);
        let card_decoration = shared.quads.blend(&device);
        let presentation =
            present::PresentationPipeline::new(&device, format, graphics_budget.clone());
        let imgs = shared
            .images
            .layer(&device, graphics_budget.clone())
            .ok_or_else(|| {
                anyhow!("GPU graphics budget exhausted while creating image pipeline")
            })?;
        let media_receipt_img = shared
            .images
            .layer_with_instance_limit(&device, graphics_budget.clone(), 1)
            .ok_or_else(|| {
                anyhow!("GPU graphics budget exhausted while creating image-receipt pipeline")
            })?;
        // Separate image layer so the wallpaper draws behind cell/chrome quads
        // (see the `bg_imgs` field docs).
        let bg_imgs = shared
            .images
            .layer_with_instance_limit(&device, graphics_budget.clone(), 1)
            .ok_or_else(|| {
                anyhow!("GPU graphics budget exhausted while creating background image pipeline")
            })?;
        // Procedural starfield wallpaper, same back-most slot.
        let starfield = starfield::StarfieldPipeline::new(&device, format);
        // Cell-locked pane-text pipeline (the `text-renderer=grid` default).
        // Always constructed; only emitted/drawn in grid mode.
        let glyph_pipeline =
            GlyphPipeline::new_with_budget(&device, format, graphics_budget.clone()).ok_or_else(
                || anyhow!("GPU graphics budget exhausted while creating glyph pipeline"),
            )?;

        Ok(Renderer {
            surface,
            gpu,
            graphics_budget,
            config,
            supported_alpha_modes,
            font_system,
            text_symbol_family,
            swash,
            atlas,
            viewport,
            text_renderer,
            glyph_pipeline,
            glyph_instances: Vec::new(),
            glyph_clips: Vec::new(),
            glyph_char_starts: Vec::new(),
            grid_glyphs_dirty: true,
            last_text_layout_key: None,
            bundled_style_faces_loaded: false,
            pane_buffer_ids: Vec::new(),
            pane_buffers: Vec::new(),
            span_scratch: Vec::new(),
            quad_scratch: Vec::new(),
            cursor_quad_range: None,
            text_prepares: 0,
            chrome_prepares: 0,
            frames_presented: 0,
            last_scene: None,
            cursor_patch: None,
            #[cfg(target_os = "macos")]
            cursor_layer: None,
            span_breaks_scratch: Vec::new(),
            minimum_contrast_cache: MinimumContrastCache::default(),
            chrome_colors: chrome::Colors::default(),
            pane_line_keys: Vec::new(),
            pane_style_keys: Vec::new(),
            line_text_scratch: String::new(),
            chrome_style_key: 0,
            last_chrome_hash: 0,
            text_prepare_dirty: true,
            last_overlay_open: false,
            cursor_glyph_renderer,
            cursor_glyph_buffer,
            pending_cursor_glyph: None,
            last_cursor_glyph_key: None,
            pane_titlebar_texts: Vec::new(),
            tab_texts: Vec::new(),
            hint_texts: Vec::new(),
            tab_close_text: String::new(),
            tabbar_text: String::new(),
            new_tab_arrow_text: String::new(),
            scroll_left_text: String::new(),
            scroll_right_text: String::new(),
            status_bar_text: String::new(),
            resize_overlay_buffer,
            resize_overlay_text: String::new(),
            update_toast_buffers,
            update_toast_text: std::array::from_fn(|_| String::new()),
            ime_buffer,
            ime_quads,
            ime_text_renderer,
            ime_prepared: false,
            ime_text: String::new(),
            pane_titlebar_buffers: Vec::new(),
            tab_buffers: Vec::new(),
            hint_buffers: Vec::new(),
            context_menu_buffers: Vec::new(),
            context_menu_texts: Vec::new(),
            context_menu_hint_buffers: Vec::new(),
            context_menu_hint_texts: Vec::new(),
            settings_buffers: Vec::new(),
            settings_texts: Vec::new(),
            settings_lines_source: None,
            settings_lines_cache: Vec::new(),
            completion_buffers: Vec::new(),
            completion_texts: Vec::new(),
            completion_spans: Vec::new(),
            completion_selected: Vec::new(),
            completion_emphasis_colors: Vec::new(),
            completion_description_buffers: Vec::new(),
            completion_description_texts: Vec::new(),
            completion_header_buffer,
            completion_header_text: String::new(),
            completion_count_buffer,
            completion_count_text: String::new(),
            media_receipt_title_buffer,
            media_receipt_title_text: String::new(),
            media_receipt_detail_buffer,
            media_receipt_detail_text: String::new(),
            media_receipt_dismiss_buffer,
            media_receipt_badge_buffer,
            tabbar_buffer,
            new_tab_arrow_buffer,
            scroll_left_buffer,
            scroll_right_buffer,
            tab_close_buffer,
            search_segment_buffers,
            search_segment_texts: vec![String::new(); SEARCH_SEGMENTS],
            search_editor_stops: Vec::new(),
            search_editor_clusters: Vec::new(),
            search_editor_rtl: false,
            status_bar_buffer,
            pane_bases,
            live_pane_bases,
            quads,
            pane_outlines,
            pane_dimming,
            text_decorations,
            overlay_quads,
            menu_quads,
            menu_text_renderer,
            rounded_surfaces,
            toast_surfaces,
            transport_surface_count: 0,
            confirmation_text_renderer,
            confirmation_text,
            confirmation_labels_prepared: false,
            imgs,
            card_base,
            card_cursors,
            card_cursor_quad_range: None,
            card_decoration,
            card_posters: None,
            lane_quads,
            lane_img: None,
            lane_text_renderer,
            lane_texts: std::collections::HashMap::new(),
            lane_labels_prepared: false,
            card_image_shared: shared.images,
            card_frames: Vec::new(),
            card_scene: card_scene::CardScene::default(),
            drawn_cards: Vec::new(),
            painted_cards: Vec::new(),
            painted_shown: Vec::new(),
            card_text_renderer,
            card_label_buffers: Vec::new(),
            card_label_texts: Vec::new(),
            card_labels_prepared: false,
            media_receipt_img,
            bg_imgs,
            starfield,
            presentation,
            starfield_started: std::time::Instant::now(),
            bg_image_cache: None,
            bg_image_retry_at: None,
            bg_image_worker: None,
            bg_image_redraw: None,
            bg_image_pending: None,
            font_family: cfg.font_family.as_str().into(),
            font_size,
            metrics,
            cell_w,
            cell_h,
            cell_scale_w,
            cell_scale_h,
            scale,
            accent_override: None,
            rounded_window_corners: false,
            live_background_opacity_floor: None,
            pending_screenshot: None,
            screenshot_worker: None,
        })
    }

    /// The shared GPU handles, for spawning another window's Renderer via
    /// [`Renderer::new_with_gpu`]. Cloning the returned context is a refcount
    /// bump.
    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    /// Snapshot the CPU-owned live state needed to rebuild this renderer.
    ///
    /// A screenshot already submitted to `screenshot_worker` owns its request
    /// and completion sender on that worker thread, so it continues to finish
    /// independently when the renderer is dropped. Only a request still queued
    /// for the next frame belongs in the recovery snapshot.
    pub fn recovery_state(&self) -> RendererRecoveryState {
        RendererRecoveryState {
            font_family: self.font_family.clone(),
            font_size: self.font_size,
            cell_scale_w: self.cell_scale_w,
            cell_scale_h: self.cell_scale_h,
            pending_screenshot: self.pending_screenshot.clone(),
        }
    }

    /// Reapply a recovery snapshot to a freshly constructed renderer.
    ///
    /// The replacement keeps the scale and surface size selected by its
    /// constructor because those values may have changed while the GPU was
    /// unavailable (for example, after moving the window to another monitor).
    /// All font-derived metrics are recomputed once at that current scale.
    pub fn restore_recovery_state(&mut self, state: &RendererRecoveryState) {
        self.last_scene = None;
        self.font_family = state.font_family.clone();
        self.font_size = clamp_font_size(state.font_size);
        self.cell_scale_w = state.cell_scale_w.max(0.01);
        self.cell_scale_h = state.cell_scale_h.max(0.01);
        self.pending_screenshot = state.pending_screenshot.clone();
        self.metrics = metrics_for(self.font_size, self.scale);
        self.remeasure_cell();
    }

    /// Tell the renderer whether the native content surface currently ends in
    /// rounded bottom corners. The app owns decoration/fullscreen state; the
    /// renderer owns pane geometry and is therefore the only layer that can
    /// round exactly the focused/inactive pane corners that meet the window.
    pub fn set_rounded_window_corners(&mut self, rounded: bool) {
        self.rounded_window_corners = rounded;
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.last_scene = None;
        // Resize recomputes wallpaper geometry without re-decoding its cached source frames.
        // `bg_image_cache` stores the DECODED image, not a window-sized
        // texture, and every frame recomputes the image rect from the current
        // surface dims (see `background_image_rect`), so a resize takes effect
        // on the next frame.
        //
        // Floor at 1 (`surface.configure(0, ...)` panics) and ceiling at the
        // device's max-texture-dimension-2d. The device requests the ADAPTER's
        // full 2D limit rather than wgpu's default 8192, so a window stretched
        // across multiple 4K monitors keeps its real physical size on hardware
        // that can present it. The ceiling stays because, on an adapter whose
        // genuine limit is smaller than the window, configuring past it fails
        // validation and leaves a stale surface that paints nothing at all.
        // Clipping to the visible top-left region is the better failure.
        // Sibling to `cap_axis_cells` (same bug class on the `--screenshot`
        // path).
        let (width, height) = live_surface_dimensions(
            width,
            height,
            self.gpu.device.limits().max_texture_dimension_2d,
        );
        self.config.width = width;
        self.config.height = height;
        self.configure_surface();
    }

    pub(crate) fn configure_surface(&self) {
        if let Some(surface) = &self.surface {
            surface.configure(&self.gpu.device, &self.config);
        }
    }

    pub fn set_background_compositing(&mut self, cfg: &Config) {
        let alpha_mode = desired_alpha_mode(cfg, &self.supported_alpha_modes);
        if self.config.alpha_mode == alpha_mode {
            return;
        }
        self.last_scene = None;
        self.config.alpha_mode = alpha_mode;
        self.configure_surface();
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }
}

/// Map the user-facing `gpu-power-preference` onto wgpu's adapter selector.
fn power_preference_of(pref: kettle_config::GpuPowerPreference) -> wgpu::PowerPreference {
    match pref {
        kettle_config::GpuPowerPreference::Low => wgpu::PowerPreference::LowPower,
        kettle_config::GpuPowerPreference::High => wgpu::PowerPreference::HighPerformance,
        kettle_config::GpuPowerPreference::Auto => wgpu::PowerPreference::None,
    }
}

/// Headless all-backend instance used by detection and cross-backend policy.
fn gpu_instance() -> wgpu::Instance {
    gpu_instance_for_backends(wgpu::Backends::all())
}

pub(crate) fn gpu_instance_for_backends(backends: wgpu::Backends) -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    wgpu::Instance::new(descriptor)
}

trait GpuDisplayHandleOwner: HasDisplayHandle + Send + Sync + 'static {}

impl<T> GpuDisplayHandleOwner for T where T: HasDisplayHandle + Send + Sync + 'static {}

/// Cloneable, type-erased owner of the platform display connection.
///
/// Kettle supplies winit's `OwnedDisplayHandle`, which owns only the event-loop
/// display connection. It deliberately does not own a window: the process-wide
/// [`GpuContext`] can therefore outlive window 1 without preventing that
/// window's native resources from being destroyed.
#[derive(Clone)]
struct OwnedGpuDisplayHandle(Arc<dyn GpuDisplayHandleOwner>);

impl OwnedGpuDisplayHandle {
    fn new<D>(display_handle: D) -> Self
    where
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        Self(Arc::new(display_handle))
    }
}

impl std::fmt::Debug for OwnedGpuDisplayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedGpuDisplayHandle")
    }
}

impl HasDisplayHandle for OwnedGpuDisplayHandle {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

/// Kettle's live path retains the display handle required by wgpu's GLES
/// backend, notably for Wayland presentation. Compatibility constructors that
/// were given only a window pass `None`; headless diagnostics intentionally use
/// [`gpu_instance_for_backends`] instead.
fn gpu_instance_for_live_display(
    display_handle: Option<&OwnedGpuDisplayHandle>,
    backends: wgpu::Backends,
) -> wgpu::Instance {
    let mut descriptor = match display_handle {
        Some(display_handle) => {
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display_handle.clone()))
        }
        None => wgpu::InstanceDescriptor::new_without_display_handle(),
    };
    descriptor.backends = backends;
    wgpu::Instance::new(descriptor)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterEscalation {
    Preferred,
    AlternateBackend,
    SurfacePreferred,
    AnyHardware,
    ForceSoftware,
}

/// Adapter-selection state grouped for display-handle-aware renderer rebuilds.
///
/// The existing [`Renderer::new_with_escalation`] signature remains available
/// for compatibility; new winit integrations pass this value alongside an
/// event-loop-owned display handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterSelection {
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
}

impl AdapterSelection {
    pub const fn new(escalation: AdapterEscalation, avoid: Option<GpuAdapterKey>) -> Self {
        Self { escalation, avoid }
    }
}

pub fn escalation_for_attempt(attempt: u32) -> AdapterEscalation {
    match attempt {
        0 => AdapterEscalation::AlternateBackend,
        1 => AdapterEscalation::SurfacePreferred,
        2 => AdapterEscalation::AnyHardware,
        _ => AdapterEscalation::ForceSoftware,
    }
}

/// A detected GPU adapter, described in kettle's own vocabulary so
/// kettle-ui (the settings GPU picker) never has to name a `wgpu` type. Carries
/// the PCI `(vendor, device)` pair the config pins on, the human display name,
/// and string-ized `kind` / `backend` for the settings list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuAdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    /// "Discrete" | "Integrated" | "Virtual" | "Software" | "Other".
    pub kind: &'static str,
    /// "DX12" | "Vulkan" | "Metal" | "GL" | "Other".
    pub backend: &'static str,
}

/// Backend-aware model key. wgpu exposes no physical-device identifier, so
/// several enumerated adapters can share this key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuAdapterKey {
    pub(crate) vendor: u32,
    pub(crate) device: u32,
    pub(crate) backend: wgpu::Backend,
}

impl GpuAdapterKey {
    pub(crate) fn from_info(info: &wgpu::AdapterInfo) -> Self {
        Self {
            vendor: info.vendor,
            device: info.device,
            backend: info.backend,
        }
    }

    fn same_model(self, info: &wgpu::AdapterInfo) -> bool {
        self.vendor == info.vendor && self.device == info.device
    }
}

pub(crate) const MAX_RECOVERY_ADAPTER_ATTEMPTS: usize = 8;

fn ambiguous_adapter_model(keys: &[GpuAdapterKey], failed: GpuAdapterKey) -> bool {
    keys.iter()
        .filter(|key| key.vendor == failed.vendor && key.device == failed.device)
        .count()
        > 1
}

pub(crate) fn recovery_candidate_indices(
    keys: &[GpuAdapterKey],
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
    preferred: Option<GpuAdapterKey>,
) -> Vec<usize> {
    let same_model =
        |a: GpuAdapterKey, b: GpuAdapterKey| a.vendor == b.vendor && a.device == b.device;
    let ambiguous = avoid.is_some_and(|failed| ambiguous_adapter_model(keys, failed));
    let mut indices: Vec<_> = keys
        .iter()
        .enumerate()
        .filter_map(|(i, &key)| {
            let eligible = match escalation {
                AdapterEscalation::AlternateBackend => avoid.is_some_and(|failed| {
                    same_model(failed, key) && (ambiguous || key.backend != failed.backend)
                }),
                AdapterEscalation::SurfacePreferred => {
                    preferred.is_some_and(|p| same_model(p, key))
                        && avoid.is_none_or(|failed| ambiguous || !same_model(failed, key))
                }
                AdapterEscalation::AnyHardware => {
                    avoid.is_none_or(|failed| ambiguous || !same_model(failed, key))
                }
                _ => false,
            };
            eligible.then_some(i)
        })
        .collect();
    if escalation == AdapterEscalation::AlternateBackend
        && let Some(failed) = avoid
    {
        // A backend can create devices successfully and fail only while drawing.
        // Keep ambiguous boards eligible, but try other backends first.
        indices.sort_by_key(|&index| keys[index].backend == failed.backend);
    }
    indices
}

pub(crate) async fn try_adapter_candidates<T: Clone, D, F, Fut>(
    candidates: Vec<T>,
    mut request: F,
) -> Result<(T, D)>
where
    F: FnMut(T) -> Fut,
    Fut: std::future::Future<Output = Result<D>>,
{
    let mut failure = anyhow!("no eligible GPU adapter");
    for candidate in candidates.into_iter().take(MAX_RECOVERY_ADAPTER_ATTEMPTS) {
        match request(candidate.clone()).await {
            Ok(device) => return Ok((candidate, device)),
            Err(error) => failure = error,
        }
    }
    Err(failure)
}

fn device_kind_str(t: wgpu::DeviceType) -> &'static str {
    match t {
        wgpu::DeviceType::DiscreteGpu => "Discrete",
        wgpu::DeviceType::IntegratedGpu => "Integrated",
        wgpu::DeviceType::VirtualGpu => "Virtual",
        wgpu::DeviceType::Cpu => "Software",
        _ => "Other",
    }
}

fn backend_str(b: wgpu::Backend) -> &'static str {
    match b {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "DX12",
        wgpu::Backend::Gl => "GL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        _ => "Other",
    }
}

fn configured_backend(b: kettle_config::GpuBackend) -> Option<wgpu::Backend> {
    use kettle_config::GpuBackend;
    match b {
        GpuBackend::Auto => None,
        GpuBackend::Dx12 => Some(wgpu::Backend::Dx12),
        GpuBackend::Vulkan => Some(wgpu::Backend::Vulkan),
        GpuBackend::Metal => Some(wgpu::Backend::Metal),
        GpuBackend::Gl => Some(wgpu::Backend::Gl),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendPlatform {
    Windows,
    MacOs,
    Other,
}

const fn host_backend_platform() -> BackendPlatform {
    if cfg!(target_os = "windows") {
        BackendPlatform::Windows
    } else if cfg!(target_os = "macos") {
        BackendPlatform::MacOs
    } else {
        BackendPlatform::Other
    }
}

const WINDOWS_BACKEND_ORDER: &[wgpu::Backend] = &[
    wgpu::Backend::Dx12,
    wgpu::Backend::Vulkan,
    wgpu::Backend::Gl,
];

const MACOS_BACKEND_ORDER: &[wgpu::Backend] = &[
    wgpu::Backend::Metal,
    wgpu::Backend::Vulkan,
    wgpu::Backend::Gl,
];

const OTHER_BACKEND_ORDER: &[wgpu::Backend] = &[wgpu::Backend::Vulkan, wgpu::Backend::Gl];

const fn native_backend_order_for(platform: BackendPlatform) -> &'static [wgpu::Backend] {
    match platform {
        BackendPlatform::Windows => WINDOWS_BACKEND_ORDER,
        BackendPlatform::MacOs => MACOS_BACKEND_ORDER,
        BackendPlatform::Other => OTHER_BACKEND_ORDER,
    }
}

pub(crate) fn backend_attempt_order_for(
    platform: BackendPlatform,
    requested: Option<wgpu::Backend>,
) -> Vec<wgpu::Backend> {
    let mut order = Vec::with_capacity(native_backend_order_for(platform).len() + 1);
    if let Some(requested) = requested {
        order.push(requested);
    }
    order.extend(
        native_backend_order_for(platform)
            .iter()
            .copied()
            .filter(|backend| Some(*backend) != requested),
    );
    order
}

fn backend_attempt_order(cfg: &Config) -> Vec<wgpu::Backend> {
    backend_attempt_order_for(host_backend_platform(), configured_backend(cfg.gpu_backend))
}

fn backend_mask(backend: wgpu::Backend) -> wgpu::Backends {
    match backend {
        wgpu::Backend::Vulkan => wgpu::Backends::VULKAN,
        wgpu::Backend::Metal => wgpu::Backends::METAL,
        wgpu::Backend::Dx12 => wgpu::Backends::DX12,
        wgpu::Backend::Gl => wgpu::Backends::GL,
        wgpu::Backend::BrowserWebGpu => wgpu::Backends::BROWSER_WEBGPU,
        _ => wgpu::Backends::empty(),
    }
}

/// Stable native-first backend order. In particular, Windows Auto deliberately
/// chooses DX12 before Vulkan: this avoids depending on wgpu enumeration order
/// and avoids a cross-vendor Vulkan ICD becoming the accidental default.
pub(crate) const fn backend_rank_for(platform: BackendPlatform, backend: wgpu::Backend) -> u8 {
    match platform {
        BackendPlatform::Windows => match backend {
            wgpu::Backend::Dx12 => 0,
            wgpu::Backend::Vulkan => 1,
            wgpu::Backend::Gl => 2,
            _ => 3,
        },
        BackendPlatform::MacOs => match backend {
            wgpu::Backend::Metal => 0,
            wgpu::Backend::Vulkan => 1,
            wgpu::Backend::Gl => 2,
            _ => 3,
        },
        BackendPlatform::Other => match backend {
            wgpu::Backend::Vulkan => 0,
            wgpu::Backend::Gl => 1,
            _ => 2,
        },
    }
}

fn backend_rank(backend: wgpu::Backend) -> u8 {
    backend_rank_for(host_backend_platform(), backend)
}

fn device_rank(device_type: wgpu::DeviceType, preference: kettle_config::GpuPowerPreference) -> u8 {
    use kettle_config::GpuPowerPreference;
    match preference {
        GpuPowerPreference::High => match device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Cpu => 3,
            _ => 4,
        },
        GpuPowerPreference::Low => match device_type {
            wgpu::DeviceType::IntegratedGpu => 0,
            wgpu::DeviceType::DiscreteGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Cpu => 3,
            _ => 4,
        },
        GpuPowerPreference::Auto => match device_type {
            wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::VirtualGpu => 1,
            wgpu::DeviceType::Cpu => 2,
            _ => 3,
        },
    }
}

pub(crate) fn adapter_priority_for(
    platform: BackendPlatform,
    backend: wgpu::Backend,
    device_type: wgpu::DeviceType,
    preference: kettle_config::GpuPowerPreference,
    preferred: bool,
) -> (u8, u8, u8) {
    use kettle_config::GpuPowerPreference;
    let backend = backend_rank_for(platform, backend);
    let device = device_rank(device_type, preference);
    let not_preferred = u8::from(!preferred);
    match preference {
        // Auto is the native-backend policy. Within one backend, wgpu's
        // surface-preferred physical adapter breaks otherwise equal hardware.
        GpuPowerPreference::Auto => (backend, device, not_preferred),
        // Explicit low/high is a physical-GPU request and must win even when
        // that GPU is available only through a lower-ranked backend. When
        // several adapters have the requested class, preserve wgpu/the
        // platform's physical-adapter preference before choosing a backend.
        GpuPowerPreference::Low | GpuPowerPreference::High => (device, not_preferred, backend),
    }
}

pub(crate) fn has_gpu_pin(cfg: &Config) -> bool {
    (cfg.gpu_vendor_id != 0 && cfg.gpu_device_id != 0) || !cfg.gpu_name.trim().is_empty()
}

pub(crate) fn can_probe_native_backend_directly(cfg: &Config) -> bool {
    !has_gpu_pin(cfg) && cfg.gpu_power_preference == kettle_config::GpuPowerPreference::Auto
}

pub(crate) fn should_query_preferred_adapter(
    escalation: AdapterEscalation,
    force_software: bool,
) -> bool {
    !force_software
        && matches!(
            escalation,
            AdapterEscalation::Preferred
                | AdapterEscalation::SurfacePreferred
                | AdapterEscalation::AnyHardware
        )
}

fn same_model(a: &wgpu::AdapterInfo, b: GpuAdapterKey) -> bool {
    b.same_model(a)
}

fn stable_adapter_cmp(a: &wgpu::AdapterInfo, b: &wgpu::AdapterInfo) -> std::cmp::Ordering {
    (
        backend_rank(a.backend),
        a.vendor,
        a.device,
        a.name.to_ascii_lowercase(),
    )
        .cmp(&(
            backend_rank(b.backend),
            b.vendor,
            b.device,
            b.name.to_ascii_lowercase(),
        ))
}

/// Enumerate the machine's GPU adapters across every backend, de-duplicated by
/// `(vendor, device, name)` so the same physical GPU exposed under multiple
/// backends (e.g. DX12 *and* Vulkan on Windows) shows once in the picker. Sort
/// first so the picker records the deterministic native backend, not whichever
/// backend the driver happened to enumerate first.
pub async fn enumerate_adapter_infos(instance: &wgpu::Instance) -> Vec<GpuAdapterInfo> {
    let mut seen: std::collections::HashSet<(u32, u32, String)> = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut adapters = instance.enumerate_adapters(wgpu::Backends::all()).await;
    adapters.sort_by(|a, b| stable_adapter_cmp(&a.get_info(), &b.get_info()));
    for a in adapters {
        let info = a.get_info();
        if !seen.insert((info.vendor, info.device, info.name.clone())) {
            continue;
        }
        out.push(GpuAdapterInfo {
            name: info.name,
            vendor: info.vendor,
            device: info.device,
            kind: device_kind_str(info.device_type),
            backend: backend_str(info.backend),
        });
    }
    out
}

/// Convenience wrapper for kettle-ui: spin up a throwaway `wgpu::Instance` and
/// return the detected GPUs. Keeps `wgpu` out of the UI crate's vocabulary and
/// stays synchronous for the settings code (blocks on the async enumeration —
/// a one-shot, off the render hot path).
pub fn detect_gpus() -> Vec<GpuAdapterInfo> {
    pollster::block_on(enumerate_adapter_infos(&gpu_instance()))
}

/// Rank an adapter against a saved pin: IDs, exact name, then name substring.
/// `u8::MAX` means no match. Shared with the Settings adapter picker.
pub fn pin_match_rank_fields(vendor: u32, device: u32, name: &str, cfg: &Config) -> u8 {
    if cfg.gpu_vendor_id != 0
        && cfg.gpu_device_id != 0
        && vendor == cfg.gpu_vendor_id
        && device == cfg.gpu_device_id
    {
        return 0;
    }
    let wanted = cfg.gpu_name.trim().to_ascii_lowercase();
    if !wanted.is_empty() {
        let actual = name.to_ascii_lowercase();
        if actual == wanted {
            return 1;
        }
        if actual.contains(&wanted) {
            return 2;
        }
    }
    u8::MAX
}

fn pin_match_rank(info: &wgpu::AdapterInfo, cfg: &Config) -> u8 {
    pin_match_rank_fields(info.vendor, info.device, &info.name, cfg)
}

fn take_best(
    mut adapters: Vec<wgpu::Adapter>,
    cfg: &Config,
    preferred: Option<GpuAdapterKey>,
) -> Option<wgpu::Adapter> {
    adapters.sort_by(|a, b| {
        let a = a.get_info();
        let b = b.get_info();
        let a_preferred = preferred.is_some_and(|p| same_model(&a, p));
        let b_preferred = preferred.is_some_and(|p| same_model(&b, p));
        adapter_priority_for(
            host_backend_platform(),
            a.backend,
            a.device_type,
            cfg.gpu_power_preference,
            a_preferred,
        )
        .cmp(&adapter_priority_for(
            host_backend_platform(),
            b.backend,
            b.device_type,
            cfg.gpu_power_preference,
            b_preferred,
        ))
        .then_with(|| stable_adapter_cmp(&a, &b))
    });
    adapters.into_iter().next()
}

pub(crate) fn effective_backend(
    requested: Option<wgpu::Backend>,
    available: &[wgpu::Backend],
) -> Option<wgpu::Backend> {
    requested.filter(|backend| available.contains(backend))
}

fn apply_backend_policy(
    adapters: Vec<wgpu::Adapter>,
    requested: Option<wgpu::Backend>,
    context: &str,
) -> Vec<wgpu::Adapter> {
    let Some(requested) = requested else {
        return adapters;
    };
    let available: Vec<_> = adapters
        .iter()
        .map(|adapter| adapter.get_info().backend)
        .collect();
    if effective_backend(Some(requested), &available).is_some() {
        return adapters
            .into_iter()
            .filter(|adapter| adapter.get_info().backend == requested)
            .collect();
    }
    log::warn!(
        "{context}: requested GPU backend {} is unavailable; falling back to the native backend order",
        backend_str(requested)
    );
    adapters
}

fn take_pinned(
    adapters: &[wgpu::Adapter],
    cfg: &Config,
    requested: Option<wgpu::Backend>,
    preferred: Option<GpuAdapterKey>,
    context: &str,
) -> Option<wgpu::Adapter> {
    if !has_gpu_pin(cfg) {
        return None;
    }
    let pin_rank = adapters
        .iter()
        .map(|adapter| pin_match_rank(&adapter.get_info(), cfg))
        .min()
        .unwrap_or(u8::MAX);
    if pin_rank == u8::MAX {
        return None;
    }
    let pinned: Vec<_> = adapters
        .iter()
        .filter(|adapter| pin_match_rank(&adapter.get_info(), cfg) == pin_rank)
        .cloned()
        .collect();
    take_best(
        apply_backend_policy(pinned, requested, context),
        cfg,
        preferred,
    )
}

async fn select_software_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    adapters: Vec<wgpu::Adapter>,
    cfg: &Config,
    preferred: Option<GpuAdapterKey>,
    requested: Option<wgpu::Backend>,
    context: &str,
) -> Result<wgpu::Adapter> {
    let software = apply_backend_policy(adapters, requested, context);
    let chosen = if let Some(chosen) = take_pinned(&software, cfg, None, preferred, context) {
        chosen
    } else if let Some(chosen) = take_best(software, cfg, preferred) {
        chosen
    } else {
        let chosen = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power_preference_of(cfg.gpu_power_preference),
                compatible_surface: surface,
                force_fallback_adapter: true,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|error| anyhow!("{context}: software adapter unavailable: {error:?}"))?;
        let info = chosen.get_info();
        if requested.is_some_and(|backend| backend != info.backend) {
            log::warn!(
                "{context}: requested software backend {} is unavailable; using {}",
                requested.map(backend_str).unwrap_or("Auto"),
                backend_str(info.backend)
            );
        }
        chosen
    };
    let info = chosen.get_info();
    log::warn!(
        "{context}: using software adapter {} ({})",
        info.name,
        backend_str(info.backend)
    );
    Ok(chosen)
}

pub(crate) async fn preferred_adapter_key(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    cfg: &Config,
) -> Option<GpuAdapterKey> {
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: power_preference_of(cfg.gpu_power_preference),
            compatible_surface: surface,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        })
        .await
        .ok()
        .map(|adapter| GpuAdapterKey::from_info(&adapter.get_info()))
}

fn log_direct_adapter_choice(context: &str, cfg: &Config, info: &wgpu::AdapterInfo) {
    if let Some(requested) = configured_backend(cfg.gpu_backend)
        && requested != info.backend
    {
        log::warn!(
            "{context}: requested GPU backend {} is unavailable; using {}",
            backend_str(requested),
            backend_str(info.backend)
        );
    }
    let software = info.device_type == wgpu::DeviceType::Cpu;
    if software {
        log::warn!(
            "{context}: using software adapter {} ({})",
            info.name,
            backend_str(info.backend)
        );
    } else {
        log::info!(
            "{context}: selected GPU {} ({}, {})",
            info.name,
            device_kind_str(info.device_type),
            backend_str(info.backend)
        );
    }
}

pub(crate) async fn request_direct_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    cfg: &Config,
    force_fallback_adapter: bool,
) -> Result<wgpu::Adapter> {
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: power_preference_of(cfg.gpu_power_preference),
            compatible_surface: surface,
            force_fallback_adapter,
            apply_limit_buckets: false,
        })
        .await
        .map_err(|error| anyhow!("{error:?}"))
}

fn direct_probe_passes(force_software: bool) -> &'static [bool] {
    if force_software {
        &[true]
    } else {
        &[false, true]
    }
}

/// Fast startup for the common unpinned Auto policy. Each instance enables one
/// backend, so a successful native request is one adapter discovery and does
/// not initialize lower-priority ICDs. Hardware across every backend is tried
/// before software fallback.
async fn resolve_window_adapter<W>(
    window: Arc<W>,
    display_handle: Option<&OwnedGpuDisplayHandle>,
    cfg: &Config,
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
    context: &str,
) -> Result<(wgpu::Instance, wgpu::Surface<'static>, Vec<wgpu::Adapter>)>
where
    W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
{
    if escalation == AdapterEscalation::Preferred && can_probe_native_backend_directly(cfg) {
        let mut failures = Vec::new();
        for &software_pass in direct_probe_passes(cfg.gpu_force_software) {
            for backend in backend_attempt_order(cfg) {
                let instance = gpu_instance_for_live_display(display_handle, backend_mask(backend));
                let surface = match instance.create_surface(window.clone()) {
                    Ok(surface) => surface,
                    Err(error) => {
                        failures.push(format!("{} surface: {error}", backend_str(backend)));
                        continue;
                    }
                };
                match request_direct_adapter(&instance, Some(&surface), cfg, software_pass).await {
                    Ok(adapter)
                        if !software_pass
                            && adapter.get_info().device_type == wgpu::DeviceType::Cpu =>
                    {
                        failures.push(format!(
                            "{} hardware pass returned a software adapter",
                            backend_str(backend)
                        ));
                    }
                    Ok(adapter) => {
                        log_direct_adapter_choice(context, cfg, &adapter.get_info());
                        return Ok((instance, surface, vec![adapter]));
                    }
                    Err(error) => {
                        failures.push(format!("{}: {error}", backend_str(backend)));
                    }
                }
            }
            if !software_pass {
                log::warn!(
                    "{context}: no hardware GPU adapter in native backend order; \
                     trying software fallback"
                );
            }
        }
        return Err(anyhow!(
            "{context}: no usable GPU adapter in native backend order: {}",
            failures.join("; ")
        ));
    }

    // Pins and low/high preference need a cross-backend view. Recovery also
    // inspects alternate backends and physical GPUs deliberately.
    let instance = gpu_instance_for_live_display(display_handle, wgpu::Backends::all());
    let surface = instance.create_surface(window)?;
    let adapter =
        resolve_adapters(&instance, Some(&surface), cfg, escalation, avoid, context).await?;
    Ok((instance, surface, adapter))
}

pub(crate) async fn resolve_headless_adapter(
    cfg: &Config,
    context: &str,
) -> Result<(wgpu::Instance, wgpu::Adapter)> {
    if can_probe_native_backend_directly(cfg) {
        let mut failures = Vec::new();
        for &software_pass in direct_probe_passes(cfg.gpu_force_software) {
            for backend in backend_attempt_order(cfg) {
                let instance = gpu_instance_for_backends(backend_mask(backend));
                match request_direct_adapter(&instance, None, cfg, software_pass).await {
                    Ok(adapter)
                        if !software_pass
                            && adapter.get_info().device_type == wgpu::DeviceType::Cpu =>
                    {
                        failures.push(format!(
                            "{} hardware pass returned a software adapter",
                            backend_str(backend)
                        ));
                    }
                    Ok(adapter) => {
                        log_direct_adapter_choice(context, cfg, &adapter.get_info());
                        return Ok((instance, adapter));
                    }
                    Err(error) => {
                        failures.push(format!("{}: {error}", backend_str(backend)));
                    }
                }
            }
            if !software_pass {
                log::warn!(
                    "{context}: no hardware GPU adapter in native backend order; \
                     trying software fallback"
                );
            }
        }
        return Err(anyhow!(
            "{context}: no usable GPU adapter in native backend order: {}",
            failures.join("; ")
        ));
    }

    let instance = gpu_instance();
    let adapters = resolve_adapters(
        &instance,
        None,
        cfg,
        AdapterEscalation::Preferred,
        None,
        context,
    )
    .await?;
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("{context}: no eligible GPU adapter"))?;
    Ok((instance, adapter))
}

/// Resolve every live, diagnostic, screenshot, self-test and recovery adapter
/// through the same deterministic policy. Explicit backend selection works
/// with or without a physical GPU pin. An unavailable explicit backend is
/// observable and falls back to native order rather than making Kettle fail to
/// start. Initial startup can fall back to software; recovery advances through
/// explicit stages so a failed backend is not silently retried forever.
pub(crate) async fn resolve_adapters(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    cfg: &Config,
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
    context: &str,
) -> Result<Vec<wgpu::Adapter>> {
    let requested = configured_backend(cfg.gpu_backend);
    let force_software = cfg.gpu_force_software || escalation == AdapterEscalation::ForceSoftware;
    let preferred = if should_query_preferred_adapter(escalation, force_software) {
        preferred_adapter_key(instance, surface, cfg).await
    } else {
        None
    };
    let mut candidates: Vec<wgpu::Adapter> = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .filter(|adapter| surface.is_none_or(|s| adapter.is_surface_supported(s)))
        .collect();

    // Resolve a visible settings/config pin before separating hardware from
    // software. CPU adapters such as llvmpipe/lavapipe are valid explicit
    // choices, and the GPU device picker offers them.
    if escalation == AdapterEscalation::Preferred && !force_software && has_gpu_pin(cfg) {
        if let Some(chosen) = take_pinned(&candidates, cfg, requested, preferred, context) {
            let info = chosen.get_info();
            log::info!(
                "{context}: using pinned GPU {} ({}, {})",
                info.name,
                device_kind_str(info.device_type),
                backend_str(info.backend)
            );
            return Ok(vec![chosen]);
        }
        log::warn!(
            "{context}: pinned GPU (vendor={:#06x} device={:#06x} name={:?}) not found among \
             {} surface-capable adapter(s); falling back to gpu-power-preference",
            cfg.gpu_vendor_id,
            cfg.gpu_device_id,
            cfg.gpu_name,
            candidates.len()
        );
    }

    let software_candidates: Vec<_> = candidates
        .iter()
        .filter(|adapter| adapter.get_info().device_type == wgpu::DeviceType::Cpu)
        .cloned()
        .collect();

    if force_software {
        return select_software_adapter(
            instance,
            surface,
            software_candidates,
            cfg,
            preferred,
            requested,
            context,
        )
        .await
        .map(|adapter| vec![adapter]);
    }

    candidates.retain(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu);
    if candidates.is_empty() {
        if escalation != AdapterEscalation::Preferred {
            return Err(anyhow!("{context}: no hardware GPU adapter available"));
        }
        log::warn!(
            "{context}: no hardware GPU adapter; retrying with software fallback \
             (llvmpipe / lavapipe / WARP)"
        );
        return select_software_adapter(
            instance,
            surface,
            software_candidates,
            cfg,
            preferred,
            requested,
            context,
        )
        .await
        .map(|adapter| vec![adapter]);
    }

    if matches!(
        escalation,
        AdapterEscalation::AlternateBackend
            | AdapterEscalation::SurfacePreferred
            | AdapterEscalation::AnyHardware
    ) {
        let keys: Vec<_> = candidates
            .iter()
            .map(|a| GpuAdapterKey::from_info(&a.get_info()))
            .collect();
        let indices = recovery_candidate_indices(&keys, escalation, avoid, preferred);
        let eligible: Vec<_> = indices.into_iter().map(|i| candidates[i].clone()).collect();
        // Model IDs cannot tell two boards apart, or prove that different
        // backends refer to the same board. Keep enumeration order within each
        // recovery tier, with a bounded number of device requests.
        if avoid.is_some_and(|failed| ambiguous_adapter_model(&keys, failed)) {
            let eligible: Vec<_> = eligible
                .into_iter()
                .take(MAX_RECOVERY_ADAPTER_ATTEMPTS)
                .collect();
            if !eligible.is_empty() {
                return Ok(eligible);
            }
        } else if let Some(chosen) = take_best(eligible, cfg, preferred) {
            return Ok(vec![chosen]);
        }
        return Err(anyhow!(
            "{context}: no eligible hardware GPU recovery candidate"
        ));
    }

    let candidates = apply_backend_policy(candidates, requested, context);
    if let Some(chosen) = take_best(candidates, cfg, preferred) {
        let info = chosen.get_info();
        log::info!(
            "{context}: selected GPU {} ({}, {})",
            info.name,
            device_kind_str(info.device_type),
            backend_str(info.backend)
        );
        return Ok(vec![chosen]);
    }

    log::warn!(
        "{context}: no hardware GPU adapter; retrying with software fallback \
         (llvmpipe / lavapipe / WARP)"
    );
    select_software_adapter(
        instance,
        surface,
        software_candidates,
        cfg,
        preferred,
        requested,
        context,
    )
    .await
    .map(|adapter| vec![adapter])
}

/// Resolve the wgpu adapter kettle would use on this machine and
/// return a human-readable diagnostic string. It uses the live renderer's
/// configured adapter policy, except no presentation surface is required
/// because this path does not create a window.
///
/// Used by `kettle --gpu-info` so a user filing a "blank window" /
/// "no GPU adapter" bug report can attach the adapter / backend /
/// driver / texture-limit details without a windowed run. The same
/// answer would otherwise require launching the binary, hitting the
/// failure mode, and digging through `RUST_LOG=info` output.
pub fn gpu_info(cfg: &Config) -> Result<String> {
    pollster::block_on(async {
        let (_instance, adapter) = resolve_headless_adapter(cfg, "gpu_info").await?;
        let info = adapter.get_info();
        let limits = adapter.limits();
        let requested_backend = match configured_backend(cfg.gpu_backend) {
            Some(backend) => backend_str(backend).to_string(),
            None if cfg!(target_os = "windows") => "Auto (DX12 preferred)".to_string(),
            None if cfg!(target_os = "macos") => "Auto (Metal preferred)".to_string(),
            None => "Auto (Vulkan preferred)".to_string(),
        };
        let backend_fallback =
            configured_backend(cfg.gpu_backend).is_some_and(|requested| requested != info.backend);
        Ok(format!(
            "Backend policy: {requested_backend}\n\
             Backend:        {:?}\n\
             Backend fallback: {}\n\
             Adapter:        {}\n\
             Adapter type:   {:?}\n\
             Driver:         {}\n\
             Driver info:    {}\n\
             Vendor (PCI):   0x{:04x}\n\
             Device (PCI):   0x{:04x}\n\
             Max texture:    {} px / side\n\
             Max buffer:     {} bytes\n\
             Max bind groups: {}",
            info.backend,
            if backend_fallback { "yes" } else { "no" },
            if info.name.is_empty() {
                "<unnamed>".to_string()
            } else {
                info.name
            },
            info.device_type,
            if info.driver.is_empty() {
                "<unknown>".to_string()
            } else {
                info.driver
            },
            if info.driver_info.is_empty() {
                "<unknown>".to_string()
            } else {
                info.driver_info
            },
            info.vendor,
            info.device,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
            limits.max_bind_groups,
        ))
    })
}
