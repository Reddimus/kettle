//! Recovering a window's renderer after the GPU device is lost.

use super::*;

impl App {
    pub(super) fn start_gpu_incident_if_needed(&mut self) {
        if self.gpu_incident_started_for_loss {
            return;
        }
        self.gpu_incident_started_for_loss = true;
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let cache = cache_dir_from_env(|key| std::env::var(key).ok());
        match IncidentLog::start(
            cache.as_deref(),
            &self.version_line,
            gpu.adapter_info(),
            gpu.fault(),
        ) {
            Ok(log) => {
                log::warn!("GPU fault diagnostics: {}", log.path().display());
                self.gpu_incident = Some(log);
            }
            Err(error) => {
                log::warn!("GPU fault diagnostics unavailable: {error}");
            }
        }
    }

    pub(super) fn stash_renderer_for_gpu_recovery(ws: &mut WindowState) {
        Self::retire_cursor_layer(ws);
        let live = ws.renderer.as_ref().map(Renderer::recovery_state);
        retain_renderer_recovery_snapshot(&mut ws.renderer_recovery, live);
        ws.renderer = None;
    }

    pub(super) fn finish_gpu_renderer_recovery(&mut self, ws: &mut WindowState) {
        ws.renderer_recovery = None;
        ws.renderer_recovery_font_size = None;
        // The accent claim may have changed while the window had no renderer.
        crate::window_accent::apply_accent(ws);
        ws.frame_recovery.clear();
        ws.pending_output_gen.clear();
        ws.pane_snapshots.clear();
        ws.pane_snapshot_keys.clear();
        ws.reuse_pane_snapshots_once = false;
        ws.output_pacer.reset();
        ws.flood_paints = 0;
        ws.last_paint = None;

        // Recompute every pane grid from the replacement renderer's restored
        // live font/cell metrics. The constructor already configured the
        // surface at the current window size and scale; RendererRebuilt also
        // resolves any DPI transition that arrived while the device was lost.
        let Some(window) = ws.window.as_ref() else {
            return;
        };
        let size = window.inner_size();
        self.reclamp_context_menu(ws, (size.width as f32, size.height as f32));
        if ws.dpi_resize_surface_suspended {
            return;
        }
        if ws.dpi_resize.on_event(DpiResizeEvent::RendererRebuilt {
            width: size.width,
            height: size.height,
        }) == DpiResizeAction::ApplyLayout
        {
            self.resize_all(ws);
        }
    }

    /// Recreate one window's surface and retained renderer resources while
    /// keeping the healthy process-wide adapter/device/queue. wgpu 30 requires
    /// this for `CurrentSurfaceTexture::Lost`; it is also the bounded repair
    /// path for non-device renderer errors whose retained damage keys may have
    /// advanced before a fallible glyph preparation step.
    pub(super) fn try_recover_window_renderer(
        &mut self,
        ws: &mut WindowState,
    ) -> Result<(), String> {
        let Some(gpu) = self.gpu.as_ref() else {
            return Err("shared GPU context is unavailable".to_string());
        };
        if gpu.is_lost() {
            return Err("shared GPU device is lost".to_string());
        }
        let Some(window) = ws.window.clone() else {
            return Err("window is unavailable".to_string());
        };
        let Some(old_renderer) = ws.renderer.as_ref() else {
            return Err("window renderer is unavailable".to_string());
        };

        let recovery_state = old_renderer.recovery_state();
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let mut replacement = Renderer::new_with_gpu(
            gpu,
            window,
            size.width.max(1),
            size.height.max(1),
            scale,
            &self.cfg,
            None,
        )
        .map_err(|error| error.to_string())?;

        // These values can diverge from Config at runtime (font zoom, cell
        // scaling, and the resolved per-window accent). A queued screenshot
        // also owns a ctl completion sender that must survive the rebuild.
        replacement.restore_recovery_state(&recovery_state);

        // Commit only after the replacement is fully initialized. A failed
        // candidate leaves the old renderer in place while the state machine
        // backs off and tries again.
        Self::retire_cursor_layer(ws);
        ws.renderer = Some(replacement);
        crate::window_accent::apply_accent(ws);
        self.reclamp_context_menu(ws, (size.width as f32, size.height as f32));
        if ws.dpi_resize_surface_suspended {
            return Ok(());
        }
        if ws.dpi_resize.on_event(DpiResizeEvent::RendererRebuilt {
            width: size.width,
            height: size.height,
        }) == DpiResizeAction::ApplyLayout
        {
            self.resize_all(ws);
        }
        Ok(())
    }

    pub(super) fn try_recover_gpu(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        escalation: kettle_render::AdapterEscalation,
        attempt: u32,
    ) -> Result<GpuRecoveryOutcome, String> {
        let Some(window) = ws.window.clone() else {
            return Err("primary window is unavailable".to_string());
        };
        let avoid = self.gpu.as_ref().map(|gpu| gpu.adapter_key());
        log::warn!("GPU recovery attempt {attempt} ({escalation:?})");
        let _gpu_init_watchdog = GpuInitWatchdog::arm();

        Self::stash_renderer_for_gpu_recovery(ws);
        for other in self.windows.values_mut() {
            Self::stash_renderer_for_gpu_recovery(other);
        }

        let cfg = self.cfg.clone();
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let mut new_renderer = match pollster::block_on(
            kettle_render::Renderer::new_with_escalation_and_display_handle(
                window.clone(),
                event_loop.owned_display_handle(),
                size.width.max(1),
                size.height.max(1),
                scale,
                &cfg,
                kettle_render::AdapterSelection::new(escalation, avoid),
            ),
        ) {
            Ok(renderer) => renderer,
            Err(err) => {
                log::error!("GPU recovery: renderer rebuild failed: {err}");
                return Err(err.to_string());
            }
        };
        restore_gpu_renderer_state(
            &mut new_renderer,
            ws.renderer_recovery.as_ref(),
            &cfg,
            ws.renderer_recovery_font_size,
        );
        let new_gpu = new_renderer.gpu().clone();
        let targets = self.windows.values().filter_map(|other| {
            let window = other.window.clone()?;
            let size = window.inner_size();
            let scale = window.scale_factor() as f32;
            Some((
                other.seq,
                (
                    window,
                    size,
                    scale,
                    other.renderer_recovery.clone(),
                    other.renderer_recovery_font_size,
                ),
            ))
        });
        let secondary_renderers =
            build_recovery_set(targets, |(window, size, scale, snapshot, font_size)| {
                let mut renderer = kettle_render::Renderer::new_with_gpu(
                    &new_gpu,
                    window,
                    size.width.max(1),
                    size.height.max(1),
                    scale,
                    &cfg,
                    None,
                )
                .map_err(|error| error.to_string())?;
                restore_gpu_renderer_state(&mut renderer, snapshot.as_ref(), &cfg, font_size);
                Ok::<_, String>(renderer)
            })
            .map_err(|errors| {
                for (seq, error) in &errors {
                    log::error!(
                        "GPU recovery: window {seq} could not rebind to the new device: {error}"
                    );
                }
                format!(
                    "{} secondary window renderer(s) could not rebind: {}",
                    errors.len(),
                    errors
                        .into_iter()
                        .map(|(seq, error)| format!("window {seq}: {error}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })?;

        // Commit the replacement only after every live window can present on
        // the candidate adapter. A partial commit would leave some windows
        // permanently blank and stop the escalation loop on a "success".
        ws.renderer = Some(new_renderer);
        let secondary_seqs: Vec<u64> = secondary_renderers.iter().map(|(seq, _)| *seq).collect();
        for (seq, renderer) in secondary_renderers {
            if let Some(other) = self.windows.get_mut(&seq) {
                other.renderer = Some(renderer);
            }
        }
        self.gpu_software_fallback = new_gpu.is_software();
        let adapter = new_gpu.adapter_info();
        let adapter_name = new_gpu.adapter_name();
        let recovery_proxy = self.proxy.clone();
        new_gpu.set_recovery_wake(kettle_render::ScreenshotRecoveryWake::new(move || {
            let _ = recovery_proxy.send_event(UserEvent::Wakeup);
        }));
        self.gpu = Some(new_gpu);

        self.finish_gpu_renderer_recovery(ws);
        for seq in secondary_seqs {
            let Some(mut other) = self.windows.remove(&seq) else {
                continue;
            };
            self.finish_gpu_renderer_recovery(&mut other);
            self.windows.insert(seq, other);
        }

        log::info!(
            "GPU recovery succeeded on {} (software={})",
            adapter_name,
            self.gpu_software_fallback
        );

        if !window_is_render_hidden(ws)
            && let Some(window) = &ws.window
        {
            window.request_redraw();
        }
        for other in self.windows.values() {
            if !window_is_render_hidden(other)
                && let Some(window) = &other.window
            {
                window.request_redraw();
            }
        }
        Ok(GpuRecoveryOutcome {
            adapter,
            secondary_window_failures: 0,
        })
    }
}

fn restore_gpu_renderer_state(
    renderer: &mut Renderer,
    snapshot: Option<&kettle_render::RendererRecoveryState>,
    cfg: &Config,
    font_size: Option<f32>,
) {
    if let Some(snapshot) = snapshot {
        renderer.restore_recovery_state(snapshot);
    }
    renderer.set_font_family(cfg.font_family.clone());
    let size = font_size.unwrap_or_else(|| renderer.font_size());
    renderer.set_font_size(size);
    renderer.set_cell_scale(cfg.cell_width, cfg.cell_height);
}

pub(super) struct GpuInitWatchdog {
    pub(super) done: Arc<std::sync::atomic::AtomicBool>,
}

impl GpuInitWatchdog {
    pub(super) fn arm() -> Self {
        let gpu_init_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let gpu_init_done = gpu_init_done.clone();
            // If the watchdog thread can't be spawned, proceed without it
            // rather than failing init.
            let _ = std::thread::Builder::new()
                .name("kettle-gpu-init-watchdog".into())
                .spawn(move || {
                    if gpu_init_timed_out(
                        &gpu_init_done,
                        std::time::Duration::from_secs(GPU_INIT_TIMEOUT_SECS),
                        std::time::Duration::from_millis(100),
                    ) {
                        log::error!(
                            "GPU initialization did not finish within {GPU_INIT_TIMEOUT_SECS}s — \
                             your graphics driver may be hung or unresponsive. Try updating the \
                             GPU driver; on Linux you can force software rendering with \
                             `LIBGL_ALWAYS_SOFTWARE=1` or pick another backend via \
                             `WGPU_BACKEND=gl`. Exiting so kettle doesn't hang invisibly."
                        );
                        // exit() skips destructors, so run_with's guard never
                        // flushes this startup's trace.
                        crate::startup_trace::flush();
                        std::process::exit(1);
                    }
                });
        }
        Self {
            done: gpu_init_done,
        }
    }
}

impl Drop for GpuInitWatchdog {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Release);
    }
}

pub(super) fn retain_renderer_recovery_snapshot<T>(retained: &mut Option<T>, live: Option<T>) {
    if retained.is_none() {
        *retained = live;
    }
}

pub(super) struct GpuRecoveryOutcome {
    pub(super) adapter: kettle_render::GpuAdapterInfo,
    pub(super) secondary_window_failures: usize,
}

type RecoveryBuilds<T> = Vec<(u64, T)>;
type RecoveryBuildErrors<E> = Vec<(u64, E)>;

pub(super) fn build_recovery_set<T, O, E>(
    targets: impl IntoIterator<Item = (u64, T)>,
    mut build: impl FnMut(T) -> std::result::Result<O, E>,
) -> std::result::Result<RecoveryBuilds<O>, RecoveryBuildErrors<E>> {
    let mut built = Vec::new();
    let mut errors = Vec::new();
    for (seq, target) in targets {
        match build(target) {
            Ok(output) => built.push((seq, output)),
            Err(error) => errors.push((seq, error)),
        }
    }
    if errors.is_empty() {
        Ok(built)
    } else {
        Err(errors)
    }
}

/// How long to let the synchronous GPU adapter+device
/// init run before treating it as a hung graphics driver. Real init is ~1.5s;
/// this is deliberately generous so a slow-but-working GPU is never killed,
/// while still bounding the worst case (a wedged driver that never returns)
/// to a clean diagnostic exit instead of an infinite invisible-window hang.
pub(super) const GPU_INIT_TIMEOUT_SECS: u64 = 30;

pub(super) const GPU_RECOVERY_SETTLE: std::time::Duration = std::time::Duration::from_millis(700);

pub(super) fn gpu_escalation_label(escalation: kettle_render::AdapterEscalation) -> &'static str {
    match escalation {
        kettle_render::AdapterEscalation::Preferred => "preferred",
        kettle_render::AdapterEscalation::AlternateBackend => "alternate_backend",
        kettle_render::AdapterEscalation::SurfacePreferred => "surface_preferred",
        kettle_render::AdapterEscalation::AnyHardware => "any_hardware",
        kettle_render::AdapterEscalation::ForceSoftware => "force_software",
    }
}

pub(super) fn gpu_recovery_backoff(attempt: u32) -> std::time::Duration {
    let ms = match attempt {
        0 | 1 => 500,
        2 => 1000,
        3 => 2000,
        _ => 4000,
    };
    std::time::Duration::from_millis(ms)
}

/// Watchdog body for the GPU-init guard. Polls `done` every `step`
/// until `timeout` elapses. Returns `true` if the timeout was reached without
/// `done` ever being observed set — i.e. the caller should treat the init as
/// hung — and `false` if `done` was seen in time (init finished, watchdog
/// stands down). Pulled out as a pure-ish helper so the timeout logic is
/// unit-testable without standing up a GPU or calling `process::exit`.
pub(super) fn gpu_init_timed_out(
    done: &std::sync::atomic::AtomicBool,
    timeout: std::time::Duration,
    step: std::time::Duration,
) -> bool {
    use std::sync::atomic::Ordering;
    let mut waited = std::time::Duration::ZERO;
    while waited < timeout {
        if done.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(step);
        waited += step;
    }
    !done.load(Ordering::Acquire)
}
