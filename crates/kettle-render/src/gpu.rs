use super::backend::cpu::{CpuBackend, CpuSurface};
use super::backend::layers::{
    ImageLayer, ImageSharedLayer, OutlineLayer, PresentLayer, QuadLayer, RoundedSurfaceLayer,
    StarfieldLayer,
};
use super::backend::{DeviceRef, RenderDevice, RendererBackend};
use super::imgpipe::CpuImageLayer;
use super::{
    Arc, Attrs, Cache, Config, DisplayHandle, GlyphPipeline, HandleError, HasDisplayHandle,
    HasWindowHandle, MinimumContrastCache, OutlinePipeline, Renderer, Result, SEARCH_SEGMENTS,
    ScreenshotRecoveryWake, ScreenshotRequest, Shaping, SharedPipelines, StartupFonts, SwashCache,
    TextAtlas, TextBuffer, TextRenderer, Viewport, Weight, Wrap, anyhow, builtin_glyphs,
    card_scene, chrome, clamp_font_size, confirm, desired_alpha_mode, fonts,
    live_surface_dimensions, metrics_for, pipeline_build::PipelineBuild, present, rounded_surface,
    starfield, upload,
};

/// The swapchain present mode for a window surface.
///
/// On Wayland, Mailbox where the surface offers it. Mesa presents FIFO there
/// through the fifo and commit-timing protocols, and on GNOME Shell 50 a commit
/// held behind them after a fullscreen change can wait with no frame callback,
/// so the window stops repainting (the resize chip stays up) until something
/// else redraws the screen. A Wayland compositor never shows a torn frame, and
/// winit already paces each redraw on the compositor's frame callback, so
/// Mailbox still draws at most once per refresh. Everywhere else, vsync.
pub(crate) fn desired_present_mode(
    supported: &[wgpu::PresentMode],
    wayland: bool,
) -> wgpu::PresentMode {
    if wayland && supported.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::AutoVsync
    }
}

/// Whether `window` is a Wayland surface.
fn display_is_wayland(window: &impl HasDisplayHandle) -> bool {
    window.display_handle().is_ok_and(|handle| {
        matches!(
            handle.as_raw(),
            raw_window_handle::RawDisplayHandle::Wayland(_)
        )
    })
}

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

// `Renderer::with_gpu` builds these pipelines on worker threads, from a clone
// of the device and the window's graphics budget, and `EarlyGpuInstance`
// hands its instance across threads.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<wgpu::Device>();
    assert_send::<kettle_core::GraphicsBudget>();
    assert_send::<OutlinePipeline>();
    assert_send::<rounded_surface::RoundedSurfacePipeline>();
    assert_send::<present::PresentationPipeline>();
    assert_send::<starfield::StarfieldPipeline>();
    assert_send::<crate::glyphpipe::GlyphShaders>();
    assert_send::<wgpu::Instance>();
    assert_send::<OwnedGpuDisplayHandle>();
};

/// Whether a renderer on `backend` compiles its independent pipelines on
/// worker threads. Not on GL: wgpu's GL backend runs every device call under
/// one context lock, held through each shader compile, so the threads would
/// only queue for it, and a thread that waits too long for it (a second
/// under WGL) panics.
pub(crate) fn pipelines_compile_on_threads(backend: wgpu::Backend) -> bool {
    backend != wgpu::Backend::Gl
}

/// The glyph pipeline's compile, started on its own thread when `on_threads`
/// and a thread can be had. Otherwise `None`: [`finish_glyph_pipeline`] then
/// builds the whole pipeline in its original order, so a budget refused at
/// an atlas returns before anything compiles.
pub(crate) fn start_glyph_build(
    on_threads: bool,
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> Option<PipelineBuild<crate::glyphpipe::GlyphShaders>> {
    if !on_threads {
        return None;
    }
    let device = device.clone();
    PipelineBuild::try_spawn("kettle-glyphs", move || {
        GlyphPipeline::compile(&device, format)
    })
}

/// The glyph pipeline, from the compile `build` started, or built whole on
/// this thread when there is none.
pub(crate) fn finish_glyph_pipeline(
    build: Option<PipelineBuild<crate::glyphpipe::GlyphShaders>>,
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    budget: kettle_core::GraphicsBudget,
) -> Option<GlyphPipeline> {
    match build {
        Some(build) => GlyphPipeline::with_shaders(device, format, budget, build.join()),
        None => GlyphPipeline::new_with_budget(device, format, budget),
    }
}

/// The fonts a new renderer starts with. Fonts loaded before the window
/// reach here already measured. Ones measured for another scale or font are
/// measured again, without enumerating the system fonts a second time.
fn take_over_fonts(fonts: Option<StartupFonts>, cfg: &Config, scale: f32) -> StartupFonts {
    match fonts {
        Some(fonts) if fonts.matches(cfg, scale) => fonts,
        Some(fonts) => fonts.remeasure(cfg, scale),
        None => StartupFonts::load(cfg, scale),
    }
}

/// The parts of a renderer that its backend builds: glyphon's objects and
/// every scene layer. The rest of the renderer is the same on both.
struct RendererLayers {
    glyphon_cache: Cache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_renderer: TextRenderer,
    ime_text_renderer: TextRenderer,
    menu_text_renderer: TextRenderer,
    confirmation_text_renderer: TextRenderer,
    cursor_glyph_renderer: TextRenderer,
    card_text_renderer: TextRenderer,
    lane_text_renderer: TextRenderer,
    pane_bases: QuadLayer,
    quads: QuadLayer,
    pane_dimming: QuadLayer,
    text_decorations: QuadLayer,
    overlay_quads: QuadLayer,
    menu_quads: QuadLayer,
    ime_quads: QuadLayer,
    card_base: QuadLayer,
    lane_quads: QuadLayer,
    card_cursors: QuadLayer,
    card_decoration: QuadLayer,
    pane_outlines: OutlineLayer,
    rounded_surfaces: RoundedSurfaceLayer,
    menu_surfaces: RoundedSurfaceLayer,
    menu_swatch_surfaces: RoundedSurfaceLayer,
    toast_surfaces: RoundedSurfaceLayer,
    presentation: PresentLayer,
    imgs: ImageLayer,
    media_receipt_img: ImageLayer,
    bg_imgs: ImageLayer,
    card_image_shared: ImageSharedLayer,
    starfield: StarfieldLayer,
    glyph_pipeline: GlyphPipeline,
}

/// Whether a renderer constructor may compile pipelines on worker threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PipelineThreads {
    /// The constructor created the device itself, so no caller can hold an
    /// error scope on it: the workers' errors reach the device's
    /// uncaptured-error handler, as this thread's would have.
    Allowed,
    /// The device is the caller's, which may have pushed error scopes.
    /// wgpu keeps those per thread, so everything is built on this one.
    CallerThread,
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
    /// instead of loading the system fonts again, and so is `early_instance`
    /// ([`EarlyGpuInstance`]) instead of creating the first Vulkan instance.
    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_display_handle<W, D>(
        window: Arc<W>,
        display_handle: D,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
        early_instance: Option<EarlyGpuInstance>,
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
            early_instance,
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
        early_instance: Option<EarlyGpuInstance>,
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
        // and a comparator benchmark can only report the total. Separating
        // adapter/device setup from the rest identifies the slow phase.
        let t_start = std::time::Instant::now();
        let wayland = display_is_wayland(&*window);
        let (instance, surface, adapter, (device, queue)) = resolve_window_adapter(
            window,
            display_handle.as_ref(),
            early_instance,
            cfg,
            selection.escalation,
            selection.avoid,
            "Renderer::new",
        )
        .await?;
        let adapter_device_ms = t_start.elapsed().as_secs_f64() * 1000.0;
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
        let t_rest = std::time::Instant::now();
        let built = Self::with_gpu_and_surface(
            gpu,
            surface,
            wayland,
            width,
            height,
            scale,
            cfg,
            fonts,
            PipelineThreads::Allowed,
        );
        // This span covers everything after device creation, INCLUDING the
        // font-system time logged separately, so do not add the two together.
        log::info!(
            "renderer init: adapter+device {adapter_device_ms:.1}ms,              surface+fonts+pipelines {:.1}ms (font init logged separately is              part of it), total {:.1}ms",
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
        let wayland = display_is_wayland(&*window);
        let surface = gpu.instance.create_surface(window)?;
        if !gpu.adapter.is_surface_supported(&surface) {
            return Err(anyhow!(
                "the shared GPU adapter cannot present to the new window's surface"
            ));
        }
        Self::with_gpu_and_surface(
            gpu.clone(),
            surface,
            wayland,
            width,
            height,
            scale,
            cfg,
            fonts,
            PipelineThreads::CallerThread,
        )
    }

    /// Shared constructor tail: everything after a surface + GPU exist
    /// (format/alpha selection, surface configure, font system, pipelines).
    #[allow(clippy::too_many_arguments)]
    fn with_gpu_and_surface(
        gpu: GpuContext,
        surface: wgpu::Surface<'static>,
        wayland: bool,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
        threads: PipelineThreads,
    ) -> Result<Renderer> {
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let present_mode = desired_present_mode(&caps.present_modes, wayland);
        Self::with_gpu(
            gpu,
            Some(surface),
            format,
            caps.alpha_modes,
            present_mode,
            width,
            height,
            scale,
            cfg,
            fonts,
            threads,
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
                wgpu::PresentMode::AutoVsync,
                width,
                height,
                scale,
                cfg,
                None,
                PipelineThreads::Allowed,
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
        present_mode: wgpu::PresentMode,
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
        threads: PipelineThreads,
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
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        if let Some(surface) = &surface {
            surface.configure(&device, &config);
        }

        // These five pipelines need only the device and the format, so a
        // renderer that created its device compiles them on their own threads
        // while this one sets up the text and the shared pipelines. Each is
        // joined where it was built before. The glyph pipeline only compiles
        // there; its budgeted atlases and buffers are still made at its join,
        // so the graphics budget is charged in the same order. On GL, or on
        // a caller's device, each is built on this thread where it was before.
        let graphics_budget = kettle_core::GraphicsBudget::default();
        let on_threads = threads == PipelineThreads::Allowed
            && pipelines_compile_on_threads(gpu.adapter.get_info().backend);
        let outline_build = PipelineBuild::start(on_threads, "kettle-outline", {
            let device = device.clone();
            move || OutlinePipeline::new(&device, format)
        });
        let rounded_surface_build = PipelineBuild::start(on_threads, "kettle-rounded", {
            let device = device.clone();
            move || rounded_surface::RoundedSurfacePipeline::new(&device, format)
        });
        let presentation_build = PipelineBuild::start(on_threads, "kettle-present", {
            let device = device.clone();
            let budget = graphics_budget.clone();
            move || present::PresentationPipeline::new(&device, format, budget)
        });
        let starfield_build = PipelineBuild::start(on_threads, "kettle-stars", {
            let device = device.clone();
            move || starfield::StarfieldPipeline::new(&device, format)
        });
        let glyph_build = start_glyph_build(on_threads, &device, format);

        let fonts = take_over_fonts(fonts, cfg, scale);
        let handle = DeviceRef::gpu(&device, &queue);
        let cache = Cache::new(handle);
        let mut atlas = TextAtlas::new(handle, &cache, format);
        let viewport = Viewport::new(handle, &cache);
        let text_renderer = TextRenderer::new(&mut atlas, handle);

        let shared = SharedPipelines::new(&device, format);
        let pane_bases = shared.quads.replace(&device);
        let quads = shared.quads.blend(&device);
        let pane_outlines = outline_build.join();
        let pane_dimming = shared.quads.blend(&device);
        let text_decorations = shared.quads.blend(&device);
        let overlay_quads = shared.quads.blend(&device);
        let menu_quads = shared.quads.blend(&device);
        let ime_quads = shared.quads.blend(&device);
        let ime_text_renderer = TextRenderer::new(&mut atlas, handle);
        let menu_text_renderer = TextRenderer::new(&mut atlas, handle);
        // Joined before the three copies below: once the first has compiled,
        // the copies together took about 1.6 ms on NVIDIA.
        let rounded_surfaces = rounded_surface_build.join();
        let menu_surfaces = rounded_surface::RoundedSurfacePipeline::with_capacity(
            &device,
            format,
            crate::overlays::MENU_SURFACES,
        );
        let menu_swatch_surfaces = rounded_surface::RoundedSurfacePipeline::with_capacity(
            &device,
            format,
            crate::overlays::MENU_SWATCH_SURFACES,
        );
        let toast_surfaces = rounded_surface::RoundedSurfacePipeline::new(&device, format);
        let confirmation_text_renderer = TextRenderer::new(&mut atlas, handle);
        let cursor_glyph_renderer = TextRenderer::new(&mut atlas, handle);
        let card_text_renderer = TextRenderer::new(&mut atlas, handle);
        let card_base = shared.quads.blend(&device);
        let lane_quads = shared.quads.blend(&device);
        let lane_text_renderer = TextRenderer::new(&mut atlas, handle);
        let card_cursors = shared.quads.blend(&device);
        let card_decoration = shared.quads.blend(&device);
        let presentation = presentation_build.join();
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
        let starfield = starfield_build.join();
        // Cell-locked pane-text pipeline (the `text-renderer=grid` default).
        // Always constructed; only emitted/drawn in grid mode.
        let glyph_pipeline =
            finish_glyph_pipeline(glyph_build, &device, format, graphics_budget.clone())
                .ok_or_else(|| {
                    anyhow!("GPU graphics budget exhausted while creating glyph pipeline")
                })?;

        let layers = RendererLayers {
            glyphon_cache: cache,
            atlas,
            viewport,
            text_renderer,
            ime_text_renderer,
            menu_text_renderer,
            confirmation_text_renderer,
            cursor_glyph_renderer,
            card_text_renderer,
            lane_text_renderer,
            pane_bases: QuadLayer::Gpu(pane_bases),
            quads: QuadLayer::Gpu(quads),
            pane_dimming: QuadLayer::Gpu(pane_dimming),
            text_decorations: QuadLayer::Gpu(text_decorations),
            overlay_quads: QuadLayer::Gpu(overlay_quads),
            menu_quads: QuadLayer::Gpu(menu_quads),
            ime_quads: QuadLayer::Gpu(ime_quads),
            card_base: QuadLayer::Gpu(card_base),
            lane_quads: QuadLayer::Gpu(lane_quads),
            card_cursors: QuadLayer::Gpu(card_cursors),
            card_decoration: QuadLayer::Gpu(card_decoration),
            pane_outlines: OutlineLayer::Gpu(pane_outlines),
            rounded_surfaces: RoundedSurfaceLayer::Gpu(rounded_surfaces),
            menu_surfaces: RoundedSurfaceLayer::Gpu(menu_surfaces),
            menu_swatch_surfaces: RoundedSurfaceLayer::Gpu(menu_swatch_surfaces),
            toast_surfaces: RoundedSurfaceLayer::Gpu(toast_surfaces),
            presentation: PresentLayer::Gpu(presentation),
            imgs: ImageLayer::Gpu(imgs),
            media_receipt_img: ImageLayer::Gpu(media_receipt_img),
            bg_imgs: ImageLayer::Gpu(bg_imgs),
            card_image_shared: ImageSharedLayer::Gpu(shared.images),
            starfield: StarfieldLayer::Gpu(starfield),
            glyph_pipeline,
        };
        Ok(Self::with_layers(
            RenderDevice::Gpu(gpu),
            surface,
            config,
            supported_alpha_modes,
            graphics_budget,
            fonts,
            layers,
            cfg,
            scale,
        ))
    }

    /// A CPU-backend renderer: the GPU constructors' renderer, with every
    /// layer a CPU twin and no wgpu instance, adapter, device or surface.
    /// `supported_alpha_modes` are those `surface` can show; with no
    /// surface the renderer draws only into its own frame.
    pub(crate) fn with_cpu(
        width: u32,
        height: u32,
        scale: f32,
        cfg: &Config,
        fonts: Option<StartupFonts>,
        supported_alpha_modes: Vec<wgpu::CompositeAlphaMode>,
        surface: Option<Box<dyn CpuSurface>>,
    ) -> Result<Renderer> {
        // What a CPU frame holds (see `backend::cpu`).
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let alpha_mode = desired_alpha_mode(cfg, &supported_alpha_modes);
        let (width, height) =
            live_surface_dimensions(width, height, crate::backend::CPU_MAX_TEXTURE_DIMENSION);
        let config = wgpu::SurfaceConfiguration {
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
        let mut surface = surface;
        if let Some(surface) = surface.as_mut() {
            surface.resize(width, height, f64::from(scale));
            surface.set_translucent(alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied);
        }
        let graphics_budget = kettle_core::GraphicsBudget::default();
        let fonts = take_over_fonts(fonts, cfg, scale);
        let handle = DeviceRef::Cpu;
        let cache = Cache::new(handle);
        let mut atlas = TextAtlas::new(handle, &cache, format);
        let viewport = Viewport::new(handle, &cache);
        let mut text = || TextRenderer::new(&mut atlas, handle);
        let (text_renderer, ime_text_renderer, menu_text_renderer) = (text(), text(), text());
        let (confirmation_text_renderer, cursor_glyph_renderer) = (text(), text());
        let (card_text_renderer, lane_text_renderer) = (text(), text());
        // The graphics budget is charged in the GPU constructor's order.
        let image_layer = |max_instances: usize, what: &str| {
            CpuImageLayer::new(graphics_budget.clone(), max_instances)
                .map(ImageLayer::Cpu)
                .ok_or_else(|| anyhow!("graphics budget exhausted while creating {what}"))
        };
        let imgs = image_layer(graphics_budget.limits().placements, "image pipeline")?;
        let media_receipt_img = image_layer(1, "image-receipt pipeline")?;
        let bg_imgs = image_layer(1, "background image pipeline")?;
        let glyph_pipeline = GlyphPipeline::new_cpu(format, graphics_budget.clone())
            .ok_or_else(|| anyhow!("graphics budget exhausted while creating glyph pipeline"))?;
        let layers = RendererLayers {
            glyphon_cache: cache,
            atlas,
            viewport,
            text_renderer,
            ime_text_renderer,
            menu_text_renderer,
            confirmation_text_renderer,
            cursor_glyph_renderer,
            card_text_renderer,
            lane_text_renderer,
            pane_bases: QuadLayer::cpu_replace(),
            quads: QuadLayer::cpu_blend(),
            pane_dimming: QuadLayer::cpu_blend(),
            text_decorations: QuadLayer::cpu_blend(),
            overlay_quads: QuadLayer::cpu_blend(),
            menu_quads: QuadLayer::cpu_blend(),
            ime_quads: QuadLayer::cpu_blend(),
            card_base: QuadLayer::cpu_blend(),
            lane_quads: QuadLayer::cpu_blend(),
            card_cursors: QuadLayer::cpu_blend(),
            card_decoration: QuadLayer::cpu_blend(),
            pane_outlines: OutlineLayer::Cpu(Vec::new()),
            rounded_surfaces: RoundedSurfaceLayer::cpu(rounded_surface::DEFAULT_CAPACITY),
            menu_surfaces: RoundedSurfaceLayer::cpu(crate::overlays::MENU_SURFACES),
            menu_swatch_surfaces: RoundedSurfaceLayer::cpu(crate::overlays::MENU_SWATCH_SURFACES),
            toast_surfaces: RoundedSurfaceLayer::cpu(rounded_surface::DEFAULT_CAPACITY),
            presentation: PresentLayer::cpu(graphics_budget.clone()),
            imgs,
            media_receipt_img,
            bg_imgs,
            card_image_shared: ImageSharedLayer::Cpu,
            starfield: StarfieldLayer::Cpu(starfield::CpuStarfield::new()),
            glyph_pipeline,
        };
        Ok(Self::with_layers(
            RenderDevice::Cpu(Some(Box::new(CpuBackend::new(surface)))),
            None,
            config,
            supported_alpha_modes,
            graphics_budget,
            fonts,
            layers,
            cfg,
            scale,
        ))
    }

    /// Every constructor's tail, once its backend's layers exist: the text
    /// buffers, metrics and the renderer itself.
    #[allow(clippy::too_many_arguments)]
    fn with_layers(
        device: RenderDevice,
        surface: Option<wgpu::Surface<'static>>,
        config: wgpu::SurfaceConfiguration,
        supported_alpha_modes: Vec<wgpu::CompositeAlphaMode>,
        graphics_budget: kettle_core::GraphicsBudget,
        fonts: StartupFonts,
        layers: RendererLayers,
        cfg: &Config,
        scale: f32,
    ) -> Renderer {
        let StartupFonts {
            mut font_system,
            text_symbol_family,
            emoji_family,
            cell: (cell_w, cell_h),
            ..
        } = fonts;
        let RendererLayers {
            glyphon_cache,
            atlas,
            viewport,
            text_renderer,
            ime_text_renderer,
            menu_text_renderer,
            confirmation_text_renderer,
            cursor_glyph_renderer,
            card_text_renderer,
            lane_text_renderer,
            pane_bases,
            quads,
            pane_dimming,
            text_decorations,
            overlay_quads,
            menu_quads,
            ime_quads,
            card_base,
            lane_quads,
            card_cursors,
            card_decoration,
            pane_outlines,
            rounded_surfaces,
            menu_surfaces,
            menu_swatch_surfaces,
            toast_surfaces,
            presentation,
            imgs,
            media_receipt_img,
            bg_imgs,
            card_image_shared,
            starfield,
            glyph_pipeline,
        } = layers;
        let cell_scale_w = cfg.cell_width.max(0.01);
        let cell_scale_h = cfg.cell_height.max(0.01);

        let swash = SwashCache::new();
        // Clamp `cfg.font_size` to the same [5.0, 72.0] range as the runtime
        // `set_font_size` path. Without this, a user config of
        // `font-size = 200` boots the renderer with 200pt cells and hits the
        // wgpu 8192px-per-side texture limit (or floods the window with one
        // giant glyph). 5.0 is below "tiny but legible"; 72.0 is "billboard".
        let font_size = clamp_font_size(cfg.font_size);
        // Physical-pixel metrics — logical font size × DPI scale.
        let metrics = metrics_for(font_size, scale);
        let builtin_strokes = builtin_glyphs::Strokes::new(
            fonts::underline_em(&mut font_system, &cfg.font_family),
            metrics.font_size,
        );
        let tabbar_buffer = TextBuffer::new(&mut font_system, metrics);
        let header_title_buffer = TextBuffer::new(&mut font_system, metrics);
        let header_title_scratch = TextBuffer::new(&mut font_system, metrics);
        let new_tab_arrow_buffer = TextBuffer::new(&mut font_system, metrics);
        let tab_tooltip_buffer = TextBuffer::new(&mut font_system, metrics);
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
        let update_toast_buffers = (0..3)
            .map(|_| TextBuffer::new(&mut font_system, metrics))
            .collect();
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

        let confirmation_text = confirm::ConfirmText::new();
        let cursor_glyph_buffer = TextBuffer::new(&mut font_system, metrics);
        Renderer {
            surface,
            device,
            graphics_budget,
            config,
            supported_alpha_modes,
            font_system,
            grapheme_text_cache: Default::default(),
            text_symbol_family,
            emoji_family,
            swash,
            atlas,
            viewport,
            text_renderer,
            glyph_pipeline,
            glyph_instances: Vec::new(),
            glyph_clips: Vec::new(),
            emoji_cells: Vec::new(),
            cursor_emoji_bounds: None,
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
            selection_backdrop_alpha: 1.0,
            chrome_colors: chrome::Colors::default(),
            pane_line_keys: Vec::new(),
            pane_style_keys: Vec::new(),
            pane_bidi: Vec::new(),
            pane_mark_shifts: Vec::new(),
            mark_cache: Default::default(),
            pane_builtins: Vec::new(),
            pane_builtin_keys: Vec::new(),
            builtin_strokes,
            builtin_mask: Vec::new(),
            drawn_bidi: Vec::new(),
            painted_bidi: Vec::new(),
            line_text_scratch: String::new(),
            chrome_style_key: 0,
            last_chrome_hash: 0,
            text_prepare_dirty: true,
            last_overlay_open: false,
            cursor_glyph_renderer,
            cursor_glyph_buffer,
            pending_cursor_glyph: None,
            cursor_glyph_cache: crate::terminal::CursorGlyphCache::default(),
            cursor_mark_images: Vec::new(),
            cursor_marks: None,
            glyphon_cache,
            pane_titlebar_texts: Vec::new(),
            tab_texts: Vec::new(),
            hint_texts: Vec::new(),
            tab_close_text: String::new(),
            tabbar_text: String::new(),
            new_tab_arrow_text: String::new(),
            scroll_left_text: String::new(),
            scroll_right_text: String::new(),
            status_bar_text: String::new(),
            scrollback_buffers: Vec::new(),
            resize_overlay_buffer,
            resize_overlay_text: String::new(),
            update_toast_buffers,
            update_toast_text: vec![String::new(); 3],
            ime_buffer,
            ime_quads,
            ime_text_renderer,
            ime_prepared: false,
            ime_text: String::new(),
            pane_titlebar_buffers: Vec::new(),
            tab_buffers: Vec::new(),
            tab_hint_buffers: Vec::new(),
            hint_buffers: Vec::new(),
            pane_status_buffers: Vec::new(),
            context_menu_buffers: Vec::new(),
            context_menu_texts: Vec::new(),
            context_menu_match_keys: Vec::new(),
            context_menu_hint_buffers: Vec::new(),
            context_menu_hint_texts: Vec::new(),
            context_menu_description_buffers: Vec::new(),
            context_menu_description_texts: Vec::new(),
            theme_footer_buffers: Vec::new(),
            theme_footer_keys: Vec::new(),
            theme_footer_key: 0,
            settings_buffers: Vec::new(),
            settings_texts: Vec::new(),
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
            tab_tooltip_buffer,
            tab_tooltip_text: String::new(),
            settings_gear_mask: Default::default(),
            header_icon_masks: Default::default(),
            header_title_face: None,
            header_title_buffer,
            header_title_scratch,
            header_title_key: None,
            header_title_width: 0.0,
            tab_bell_mask: Default::default(),
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
            quads,
            pane_outlines,
            pane_dimming,
            text_decorations,
            overlay_quads,
            menu_quads,
            menu_text_renderer,
            rounded_surfaces,
            toast_surfaces,
            menu_surfaces,
            menu_swatch_surfaces,
            menu_level_draws: Vec::new(),
            menu_level_text: Vec::new(),
            transport_surface_count: 0,
            settings_surface_count: 0,
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
            card_image_shared,
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
            pending_screenshot: None,
            screenshot_worker: None,
            screenshot_retirements: Default::default(),
        }
    }

    /// The shared GPU handles, for spawning another window's Renderer via
    /// [`Renderer::new_with_gpu`]. Cloning the returned context is a refcount
    /// bump.
    ///
    /// # Panics
    ///
    /// On a [`RendererBackend::Cpu`] renderer, which has no GPU context. Every
    /// public constructor builds a GPU renderer; use
    /// [`Renderer::gpu_context`] where either backend may be in use.
    pub fn gpu(&self) -> &GpuContext {
        self.device
            .gpu()
            .expect("a GPU renderer: CPU renderers have no GPU context")
    }

    /// The shared GPU handles, or `None` on the CPU backend.
    pub fn gpu_context(&self) -> Option<&GpuContext> {
        self.device.gpu()
    }

    /// Which backend draws this renderer's frames.
    pub fn backend(&self) -> RendererBackend {
        self.device.backend()
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
        let (width, height) =
            live_surface_dimensions(width, height, self.device.max_texture_dimension_2d());
        self.config.width = width;
        self.config.height = height;
        if let RenderDevice::Cpu(Some(cpu)) = &mut self.device
            && let Some(surface) = cpu.surface.as_mut()
        {
            surface.resize(width, height, f64::from(self.scale));
        }
        self.configure_surface();
    }

    pub(crate) fn configure_surface(&self) {
        if let (Some(surface), Some(gpu)) = (&self.surface, self.device.gpu()) {
            surface.configure(&gpu.device, &self.config);
        }
    }

    pub fn set_background_compositing(&mut self, cfg: &Config) {
        let alpha_mode = desired_alpha_mode(cfg, &self.supported_alpha_modes);
        if self.config.alpha_mode == alpha_mode {
            return;
        }
        self.last_scene = None;
        self.config.alpha_mode = alpha_mode;
        if let RenderDevice::Cpu(Some(cpu)) = &mut self.device
            && let Some(surface) = cpu.surface.as_mut()
        {
            surface.set_translucent(alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied);
        }
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
    wgpu::Instance::new(instance_descriptor(backends, None))
}

/// wgpu's default instance flags without `VALIDATION_INDIRECT_CALL`.
///
/// With that flag, every device builds two compute pipelines at creation to
/// check the arguments of indirect draws and dispatches. Kettle issues none
/// (`crates/kettle-test-support/tests/gpu_indirect_calls.rs` keeps it that
/// way), so they only cost startup: about 19 ms of `request_device` on NVIDIA.
/// Debug builds keep wgpu's other debugging and validation flags.
pub(crate) fn kettle_instance_flags() -> wgpu::InstanceFlags {
    wgpu::InstanceFlags::default().difference(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL)
}

/// The descriptor of every instance Kettle creates.
pub(crate) fn instance_descriptor(
    backends: wgpu::Backends,
    display_handle: Option<&OwnedGpuDisplayHandle>,
) -> wgpu::InstanceDescriptor {
    let mut descriptor = match display_handle {
        Some(display_handle) => {
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display_handle.clone()))
        }
        None => wgpu::InstanceDescriptor::new_without_display_handle(),
    };
    descriptor.backends = backends;
    descriptor.flags = kettle_instance_flags();
    descriptor
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
pub(crate) struct OwnedGpuDisplayHandle(Arc<dyn GpuDisplayHandleOwner>);

impl OwnedGpuDisplayHandle {
    pub(crate) fn new<D>(display_handle: D) -> Self
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
    wgpu::Instance::new(instance_descriptor(backends, display_handle))
}

/// The first window's Vulkan instance, created on its own thread while the
/// config is applied and the window comes up.
///
/// `vkCreateInstance` needs neither a window nor a surface, and it is the
/// longest single step of GPU init (about 125 ms on NVIDIA), so the first
/// window's Vulkan attempt waits only for what is left of it. The instance is
/// the one that attempt would create itself: Vulkan only, Kettle's flags, and
/// the event loop's display handle, which it keeps for the life of the GPU
/// context. It is started only where wgpu has Vulkan and it is that first
/// attempt (`early_vulkan_instance_applies_for`), so `gpu-backend = gl`, a
/// GPU pin, macOS and Windows never load the Vulkan ICD early. One nobody
/// takes is destroyed on its own thread (`EarlyValue`).
pub struct EarlyGpuInstance(EarlyValue<wgpu::Instance>);

impl EarlyGpuInstance {
    /// Start creating the instance for `cfg`, or `None` when the first
    /// window would not try Vulkan first or no thread can be started. Either
    /// way the first window then creates every instance itself.
    pub fn start<D>(cfg: &Config, display_handle: D) -> Option<Self>
    where
        D: HasDisplayHandle + Send + Sync + 'static,
    {
        if !early_vulkan_instance_applies_for(
            host_backend_platform(),
            cfg,
            wgpu::Instance::enabled_backend_features(),
        ) {
            return None;
        }
        let display_handle = OwnedGpuDisplayHandle::new(display_handle);
        Self::spawn(move || {
            gpu_instance_for_live_display(Some(&display_handle), wgpu::Backends::VULKAN)
        })
    }

    pub(crate) fn spawn(create: impl FnOnce() -> wgpu::Instance + Send + 'static) -> Option<Self> {
        EarlyValue::spawn("kettle-gpu-init", create, UNUSED_EARLY_INSTANCE_WAIT).map(Self)
    }

    /// The instance, once it exists, or `None` if its thread panicked (where
    /// panics unwind, as in tests; the release profile aborts on it, as it
    /// did inline); the caller then creates its own.
    pub(crate) fn take(self) -> Option<wgpu::Instance> {
        let waited = std::time::Instant::now();
        let instance = self.0.take();
        match &instance {
            Some(_) => log::info!(
                "renderer init: waited {:.1}ms for the early Vulkan instance",
                waited.elapsed().as_secs_f64() * 1000.0
            ),
            None => log::debug!("the early Vulkan instance thread panicked"),
        }
        instance
    }
}

/// How long an unused early instance's thread is waited for: several times
/// what `vkCreateInstance` took on NVIDIA (about 125 ms).
const UNUSED_EARLY_INSTANCE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// A value made on its own thread and handed over through a rendezvous.
///
/// One nobody takes is dropped by the thread that made it, never by its
/// owner: dropping this lets the thread's hand-over fail, the thread drops
/// the value and its function returns, and the owner waits for that at most
/// `unused_wait`. The thread is never joined: whatever runs after its
/// function, such as a driver's thread-local teardown, or a thread still
/// busy after the wait, inside a hung driver say, finishes on its own. That
/// bound matters when startup fails before window 1's renderer, outside the
/// GPU-init watchdog.
pub(crate) struct EarlyValue<T> {
    handover: Option<std::sync::mpsc::Receiver<T>>,
    thread: Option<std::thread::JoinHandle<()>>,
    unused_wait: std::time::Duration,
}

impl<T: Send + 'static> EarlyValue<T> {
    pub(crate) fn spawn(
        name: &str,
        make: impl FnOnce() -> T + Send + 'static,
        unused_wait: std::time::Duration,
    ) -> Option<Self> {
        let (handover, receiver) = std::sync::mpsc::sync_channel(0);
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // With no receiver left, the value comes back and is
                // dropped here, on this thread.
                let _ = handover.send(make());
            })
            .map_err(|error| log::debug!("no {name} thread: {error}"))
            .ok()?;
        Some(Self {
            handover: Some(receiver),
            thread: Some(thread),
            unused_wait,
        })
    }

    /// The value, once it is made, or `None` if its thread panicked first.
    pub(crate) fn take(mut self) -> Option<T> {
        // Taken, the thread has nothing left to wait for, so its handle is
        // dropped rather than joined.
        self.thread = None;
        self.handover.take()?.recv().ok()
    }
}

impl<T> Drop for EarlyValue<T> {
    fn drop(&mut self) {
        drop(self.handover.take());
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = std::time::Instant::now() + self.unused_wait;
        while !thread.is_finished() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                log::warn!("left an unused early GPU value to its thread");
                return;
            }
            std::thread::sleep(left.min(std::time::Duration::from_millis(5)));
        }
    }
}

/// The instance for one direct-probe attempt on `backend`: the early one for
/// the first Vulkan attempt, unless its thread failed, and otherwise the one
/// `create` makes, as every attempt made before.
pub(crate) fn instance_for_attempt(
    backend: wgpu::Backend,
    early: &mut Option<EarlyGpuInstance>,
    create: impl FnOnce() -> wgpu::Instance,
) -> wgpu::Instance {
    early
        .take_if(|_| backend == wgpu::Backend::Vulkan)
        .and_then(EarlyGpuInstance::take)
        .unwrap_or_else(create)
}

/// Whether the first window's first GPU attempt on `platform` is a Vulkan
/// instance of its own: wgpu was built with Vulkan (`compiled`), and it is
/// the unpinned Auto-preference direct probe with Vulkan first in its
/// backend order. Windows and macOS try DX12 or Metal first, and a
/// `gpu-backend` other than Vulkan or Auto leads with that.
pub(crate) fn early_vulkan_instance_applies_for(
    platform: BackendPlatform,
    cfg: &Config,
    compiled: wgpu::Backends,
) -> bool {
    compiled.contains(wgpu::Backends::VULKAN)
        && platform == BackendPlatform::Other
        && can_probe_native_backend_directly(cfg)
        && backend_attempt_order_for(platform, configured_backend(cfg.gpu_backend)).first()
            == Some(&wgpu::Backend::Vulkan)
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

pub(crate) async fn try_discovered_devices<C, T, D, F, Fut, R, DeviceFuture>(
    candidates: impl IntoIterator<Item = C>,
    mut discover: F,
    mut request: R,
) -> Result<(T, D)>
where
    F: FnMut(C) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
    R: FnMut(T) -> DeviceFuture,
    DeviceFuture: std::future::Future<Output = Result<(T, D)>>,
{
    let mut failure = anyhow!("no eligible GPU adapter");
    for candidate in candidates {
        match discover(candidate).await {
            Ok(adapter) => match request(adapter).await {
                Ok(device) => return Ok(device),
                Err(error) => failure = error,
            },
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

async fn request_live_device(adapter: wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue)> {
    // Request the real 2D limit so high-DPI surfaces fit the device.
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("kettle-device"),
            required_features: upload::mapped_upload_features(&adapter),
            required_limits: live_device_limits(adapter.limits()),
            ..Default::default()
        })
        .await
        .map_err(|error| {
            let info = adapter.get_info();
            log::warn!(
                "skipping GPU {} ({}): device creation failed: {error:?}",
                info.name,
                backend_str(info.backend)
            );
            anyhow!("failed to create device: {error:?}")
        })
}

/// Fast startup for the common unpinned Auto policy. Each instance enables one
/// backend, so a successful native request is one adapter discovery and does
/// not initialize lower-priority ICDs. Hardware across every backend is tried
/// before software fallback. The first Vulkan attempt uses `early_instance`
/// when there is one; every other attempt creates its own instance.
async fn resolve_window_adapter<W>(
    window: Arc<W>,
    display_handle: Option<&OwnedGpuDisplayHandle>,
    early_instance: Option<EarlyGpuInstance>,
    cfg: &Config,
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
    context: &str,
) -> Result<(
    wgpu::Instance,
    wgpu::Surface<'static>,
    wgpu::Adapter,
    (wgpu::Device, wgpu::Queue),
)>
where
    W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
{
    let mut early_instance = early_instance;
    if escalation == AdapterEscalation::Preferred && can_probe_native_backend_directly(cfg) {
        let mut failures = Vec::new();
        for &software_pass in direct_probe_passes(cfg.gpu_force_software) {
            let result = try_discovered_devices(
                backend_attempt_order(cfg),
                |backend| {
                    let window = window.clone();
                    let instance = instance_for_attempt(backend, &mut early_instance, || {
                        gpu_instance_for_live_display(display_handle, backend_mask(backend))
                    });
                    async move {
                        let surface = instance.create_surface(window)?;
                        let adapter =
                            request_direct_adapter(&instance, Some(&surface), cfg, software_pass)
                                .await?;
                        if !software_pass && adapter.get_info().device_type == wgpu::DeviceType::Cpu
                        {
                            return Err(anyhow!(
                                "{} hardware pass returned a software adapter",
                                backend_str(backend)
                            ));
                        }
                        Ok((instance, surface, adapter))
                    }
                },
                |(instance, surface, adapter)| async move {
                    let device = request_live_device(adapter.clone()).await?;
                    Ok(((instance, surface, adapter), device))
                },
            )
            .await;
            match result {
                Ok(((instance, surface, adapter), device)) => {
                    log_direct_adapter_choice(context, cfg, &adapter.get_info());
                    return Ok((instance, surface, adapter, device));
                }
                Err(error) => failures.push(error.to_string()),
            }
            if !software_pass {
                log::warn!(
                    "{context}: no usable hardware GPU device in native backend order; trying software fallback"
                );
            }
        }
        return Err(anyhow!(
            "{context}: no usable GPU device in native backend order: {}",
            failures.join("; ")
        ));
    }

    // Only the direct probe has a Vulkan-only attempt to give it to.
    drop(early_instance);
    let instance = gpu_instance_for_live_display(display_handle, wgpu::Backends::all());
    let surface = instance.create_surface(window)?;
    let mut remaining: Vec<_> = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .filter(|adapter| adapter.is_surface_supported(&surface))
        .collect();
    let mut failure = anyhow!("{context}: no eligible GPU device");
    let mut attempts = 0;
    while attempts < MAX_RECOVERY_ADAPTER_ATTEMPTS {
        let adapters = match resolve_adapter_candidates(
            &instance,
            Some(&surface),
            cfg,
            escalation,
            avoid,
            context,
            remaining.clone(),
        )
        .await
        {
            Ok(adapters) => adapters
                .into_iter()
                .take(MAX_RECOVERY_ADAPTER_ATTEMPTS - attempts)
                .collect::<Vec<_>>(),
            Err(error) => return Err(failure.context(error)),
        };
        attempts += adapters.len();
        let exhausted = adapters.iter().any(|adapter| !remaining.contains(adapter));
        match try_adapter_candidates(adapters.clone(), |adapter| async move {
            request_live_device(adapter).await
        })
        .await
        {
            Ok((adapter, device)) => return Ok((instance, surface, adapter, device)),
            Err(error) => failure = error,
        }
        remaining.retain(|candidate| !adapters.contains(candidate));
        // A forced fallback absent from enumeration cannot be selected again.
        if exhausted {
            break;
        }
    }
    Err(failure)
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
    let candidates: Vec<wgpu::Adapter> = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .filter(|adapter| surface.is_none_or(|s| adapter.is_surface_supported(s)))
        .collect();

    resolve_adapter_candidates(
        instance, surface, cfg, escalation, avoid, context, candidates,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn resolve_adapter_candidates(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    cfg: &Config,
    escalation: AdapterEscalation,
    avoid: Option<GpuAdapterKey>,
    context: &str,
    mut candidates: Vec<wgpu::Adapter>,
) -> Result<Vec<wgpu::Adapter>> {
    let requested = configured_backend(cfg.gpu_backend);
    let force_software = cfg.gpu_force_software || escalation == AdapterEscalation::ForceSoftware;
    let preferred = if should_query_preferred_adapter(escalation, force_software) {
        preferred_adapter_key(instance, surface, cfg).await
    } else {
        None
    };
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
///
/// The first line says which renderer windows use (`renderer`). On the CPU
/// renderer the GPU section describes what `renderer = gpu`, or the fallback,
/// would use, and a host with no adapter is reported rather than an error.
pub fn gpu_info(cfg: &Config) -> Result<String> {
    let choice = crate::backend::preferred_backend(cfg);
    let adapter = adapter_info_text(cfg);
    renderer_info_text(cfg, choice, adapter)
}

/// `--gpu-info`'s text: the renderer, then the GPU adapter's details.
pub(crate) fn renderer_info_text(
    cfg: &Config,
    choice: crate::BackendChoice,
    adapter: Result<String>,
) -> Result<String> {
    let mut out = match choice.backend {
        crate::RendererBackend::Cpu => format!(
            "Renderer:       CPU (renderer = {}): frames are drawn on the CPU and presented \
             through shared memory (wl_shm on Wayland, MIT-SHM on X11). Windows load no GPU \
             driver; if the presenter cannot start they use the GPU below.\n",
            cfg.renderer.as_str()
        ),
        crate::RendererBackend::Gpu if cfg!(target_os = "linux") => format!(
            "Renderer:       GPU (renderer = {}). If no GPU adapter works, windows draw on \
             the CPU.\n",
            cfg.renderer.as_str()
        ),
        crate::RendererBackend::Gpu => format!(
            "Renderer:       GPU (renderer = {})\n",
            cfg.renderer.as_str()
        ),
    };
    if let Some(note) = choice.note {
        out.push_str(&format!("Renderer note:  {note}\n"));
    }
    match adapter {
        Ok(adapter) => out.push_str(&adapter),
        Err(error) if choice.backend == crate::RendererBackend::Cpu => {
            out.push_str(&format!("GPU adapter:    none usable ({error:#})"));
        }
        Err(error) => return Err(error),
    }
    Ok(out)
}

/// The adapter the GPU renderer would select, as `--gpu-info` prints it.
fn adapter_info_text(cfg: &Config) -> Result<String> {
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
