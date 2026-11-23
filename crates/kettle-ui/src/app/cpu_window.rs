//! Windows whose frames are drawn on the CPU (Linux's default renderer).
//!
//! The `renderer` key picks the backend once, for window 1
//! (`kettle_render::preferred_backend`); every later window follows it. A
//! CPU window has no GPU context, so the app's `gpu` stays `None` and
//! `cpu_rendering` records that windows can still be opened. Its presenter
//! needs three things from the event loop:
//!
//! - a frame held for the compositor's frame callback is committed when the
//!   loop wakes ([`Renderer::flush_held_frame`]);
//! - a spare buffer is released once it has been idle
//!   ([`Renderer::idle_release_deadline`]);
//! - occlusion is forwarded, so a hidden window keeps no buffer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kettle_render::{Renderer, RendererBackend};
use winit::window::Window;

use super::App;
use crate::window_state::WindowState;

/// How long a held frame may wait before the loop tries it again when no
/// frame callback has woken it: the presenter's own callback timeout.
const HELD_FRAME_RETRY: Duration = Duration::from_secs(1);

impl App {
    /// Whether window 1's renderer is up, so later windows, the quick
    /// terminal and activation requests can open windows.
    pub(super) fn renderers_ready(&self) -> bool {
        self.gpu.is_some() || self.cpu_rendering
    }

    /// Service a CPU window's presenter before the loop sleeps. Returns when
    /// it next needs the loop.
    pub(super) fn tick_cpu_presenter(ws: &mut WindowState, now: Instant) -> Option<Instant> {
        let window = ws.window.clone();
        let renderer = ws.renderer.as_mut()?;
        if renderer.backend() != RendererBackend::Cpu {
            return None;
        }
        cpu_presenter_tick(renderer, now, || {
            if let Some(window) = &window {
                window.pre_present_notify();
            }
        })
    }
}

/// The presenter half of [`App::tick_cpu_presenter`], for any renderer.
fn cpu_presenter_tick(
    renderer: &mut Renderer,
    now: Instant,
    pre_present: impl FnOnce(),
) -> Option<Instant> {
    if renderer.has_held_frame() {
        renderer.flush_held_frame(pre_present);
    }
    if renderer
        .idle_release_deadline()
        .is_some_and(|deadline| deadline <= now)
    {
        let released = renderer.release_idle_buffers();
        log::debug!("renderer: released {released} idle CPU buffer(s)");
    }
    let held = renderer.has_held_frame().then_some(now + HELD_FRAME_RETRY);
    match (renderer.idle_release_deadline(), held) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// A CPU renderer for `window`, presenting through shared memory. Fails
/// where there is no such presenter (any platform but Linux, or a display
/// without `wl_shm` or X11 SHM); the caller then uses the GPU.
pub(super) fn cpu_window_renderer<D>(
    window: &Arc<Window>,
    display: D,
    cfg: &kettle_config::Config,
    fonts: Option<kettle_render::StartupFonts>,
) -> anyhow::Result<Renderer>
where
    D: winit::raw_window_handle::HasDisplayHandle + Send + Sync + 'static,
{
    let size = window.inner_size();
    let scale = window.scale_factor() as f32;
    #[cfg(target_os = "linux")]
    {
        Renderer::new_cpu_window(
            window.clone(),
            display,
            size.width.max(1),
            size.height.max(1),
            scale,
            cfg,
            fonts,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (display, cfg, fonts, size, scale);
        Err(anyhow::anyhow!(
            "the CPU renderer is available on Linux only"
        ))
    }
}

/// On Linux, a GPU renderer that failed to start, or started only on a
/// software adapter (llvmpipe or lavapipe), is replaced by the CPU renderer:
/// it is faster than either and loads no driver. Elsewhere, and when the CPU
/// renderer cannot start either, the GPU result stands.
pub(super) fn gpu_or_cpu_fallback(
    gpu: anyhow::Result<Renderer>,
    cpu: impl FnOnce() -> anyhow::Result<Renderer>,
) -> anyhow::Result<Renderer> {
    if !cfg!(target_os = "linux") {
        return gpu;
    }
    let reason = match &gpu {
        Ok(renderer) if renderer.gpu_context().is_some_and(|gpu| gpu.is_software()) => {
            "the GPU adapter is a software rasterizer".to_string()
        }
        Ok(_) => return gpu,
        Err(error) => format!("no GPU adapter works ({error:#})"),
    };
    match cpu() {
        Ok(renderer) => {
            log::warn!("renderer: {reason}; drawing on the CPU instead");
            Ok(renderer)
        }
        Err(error) => {
            log::warn!("renderer: {reason}, and the CPU renderer could not start ({error:#})");
            gpu
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu_renderer() -> Renderer {
        let cfg = kettle_config::Config::default();
        Renderer::new_cpu_offscreen(64, 32, 1.0, &cfg, None).expect("CPU renderer")
    }

    #[test]
    fn a_renderer_without_a_presenter_needs_no_wake() {
        let mut renderer = cpu_renderer();
        let wake = cpu_presenter_tick(&mut renderer, Instant::now(), || {
            panic!("nothing is held, so nothing is committed")
        });
        assert_eq!(wake, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_gpu_that_fails_falls_back_to_the_cpu_on_linux() {
        let renderer =
            gpu_or_cpu_fallback(Err(anyhow::anyhow!("no adapter")), || Ok(cpu_renderer()))
                .expect("the CPU renderer stands in");
        assert_eq!(renderer.backend(), RendererBackend::Cpu);
        let both = gpu_or_cpu_fallback(Err(anyhow::anyhow!("no adapter")), || {
            Err(anyhow::anyhow!("no wl_shm"))
        });
        let error = both.err().expect("neither renderer starts");
        assert!(
            error.to_string().contains("no adapter"),
            "the GPU error is kept"
        );
    }

    #[test]
    fn a_working_renderer_is_kept_without_trying_the_cpu() {
        let renderer = gpu_or_cpu_fallback(Ok(cpu_renderer()), || {
            panic!("a renderer with no software GPU adapter is kept")
        })
        .expect("kept");
        assert_eq!(renderer.backend(), RendererBackend::Cpu);
    }

    #[test]
    fn the_cpu_window_hooks_are_wired_into_the_loop() {
        let app = include_str!("../app.rs");
        let tick = app
            .split_once("fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {")
            .expect("about_to_wait")
            .1;
        assert!(
            tick.contains("Self::tick_cpu_presenter(&mut ws, now)"),
            "about_to_wait services each CPU window's presenter"
        );
        let events = include_str!("window_events.rs");
        let occluded = events
            .split_once("WindowEvent::Occluded(occluded) => {")
            .expect("occlusion handler")
            .1;
        assert!(
            occluded[..occluded.find("WindowEvent::Ime").expect("next arm")]
                .contains("renderer.set_occluded(occluded)"),
            "occlusion reaches the renderer's presenter"
        );
    }

    /// Nothing loads a GPU driver into a process whose windows draw on the
    /// CPU: not the early Vulkan instance, and not the Settings adapter list.
    #[test]
    fn a_cpu_app_skips_the_early_instance_and_adapter_discovery() {
        let startup = include_str!("startup.rs");
        let early = startup
            .split_once("let early_gpu_instance = ")
            .expect("the early instance")
            .1;
        let gate = early.find("RendererBackend::Cpu").expect("a CPU gate");
        let start = early.find("EarlyGpuInstance::start(").expect("the start");
        assert!(
            gate < start,
            "the CPU check comes before the instance starts"
        );
        let actions = include_str!("actions.rs");
        let open = actions
            .split_once("fn open_settings(")
            .expect("open_settings")
            .1;
        let skip = open.find("if self.cpu_rendering").expect("a CPU skip");
        let discovery = open.find("GPU_DISCOVERY").expect("discovery");
        assert!(skip < discovery, "a CPU app never lists adapters");
    }
}
