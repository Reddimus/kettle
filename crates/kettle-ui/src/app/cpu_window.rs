//! Windows whose frames are drawn on the CPU (Linux's default renderer).
//!
//! The `renderer` key picks the backend once, for window 1
//! (`kettle_render::preferred_backend`); every later window follows it. A
//! CPU window has no GPU context, so the app's `gpu` stays `None` and
//! `cpu_rendering` records that windows can still be opened. Its presenter
//! needs four things from the event loop, all on the loop's own thread:
//!
//! - a frame held for the compositor's frame callback is committed when the
//!   loop wakes ([`Renderer::flush_held_frame`]);
//! - a frame that was not drawn because the compositor had not taken the
//!   last one (`FrameOutcome::Paced`) is redrawn, from the latest state, once
//!   it can be ([`Renderer::cpu_frame_ready`]);
//! - for both, the loop waits with `ControlFlow::WaitUntil`
//!   ([`Renderer::cpu_wake_deadline`]): under plain `Wait`, winit 0.30 does
//!   not run `about_to_wait` for a wake that brought only the presenter's
//!   frame callback, and the frame would stay off screen until some other
//!   event;
//! - a spare buffer is released once it has been idle
//!   ([`Renderer::idle_release_deadline`], recomputed on every wake), and
//!   occlusion is forwarded, so a hidden window keeps no buffer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kettle_render::{HeldFrameFlush, Renderer, RendererBackend};
use winit::window::Window;

use super::App;
use crate::window_state::WindowState;

/// The wake a presenter deadline gets when it is not in the future, which
/// the presenter's contract rules out: a floor, so a broken one cannot spin
/// the loop.
const PAST_DEADLINE_FLOOR: Duration = Duration::from_millis(16);

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
        let tick = cpu_presenter_tick(renderer, now, || {
            if let Some(window) = &window {
                window.pre_present_notify();
            }
        });
        if tick.committed
            && ws
                .card_sightings
                .note(renderer.painted_cards(), Instant::now())
        {
            // The held frame put its cards on screen.
            ws.accessibility_pending = true;
            ws.card_hover_stale = true;
        }
        if tick.redraw
            && let Some(window) = &window
        {
            window.request_redraw();
        }
        tick.wake
    }
}

/// What one presenter tick did.
#[derive(Debug, Default, PartialEq, Eq)]
struct PresenterTick {
    /// A held frame was committed.
    committed: bool,
    /// A paced frame can be drawn now: the window should redraw.
    redraw: bool,
    /// When the loop must wake for the presenter next.
    wake: Option<Instant>,
}

/// The presenter half of [`App::tick_cpu_presenter`], for any renderer.
fn cpu_presenter_tick(
    renderer: &mut Renderer,
    now: Instant,
    pre_present: impl FnOnce(),
) -> PresenterTick {
    let mut tick = PresenterTick::default();
    if renderer.has_held_frame() {
        tick.committed = renderer.flush_held_frame(pre_present) == HeldFrameFlush::Presented;
    }
    tick.redraw = renderer.cpu_frame_ready();
    if renderer
        .idle_release_deadline()
        .is_some_and(|deadline| deadline <= now)
    {
        let released = renderer.release_idle_buffers();
        log::debug!("renderer: released {released} idle CPU buffer(s)");
    }
    // Recomputed after the release: the presenter keeps a spare it could
    // not release on a deadline that stays put until it comes due.
    let idle = renderer.idle_release_deadline();
    let frame = renderer.cpu_wake_deadline();
    tick.wake = [idle, frame]
        .into_iter()
        .flatten()
        .map(|deadline| future_or_floor(deadline, now))
        .min();
    tick
}

/// `deadline`, or [`PAST_DEADLINE_FLOOR`] from `now` when it is not in the
/// future.
fn future_or_floor(deadline: Instant, now: Instant) -> Instant {
    if deadline > now {
        deadline
    } else {
        now + PAST_DEADLINE_FLOOR
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
        let tick = cpu_presenter_tick(&mut renderer, Instant::now(), || {
            panic!("nothing is held, so nothing is committed")
        });
        assert_eq!(tick, PresenterTick::default());
    }

    #[test]
    fn a_presenter_deadline_is_kept_unless_it_has_passed() {
        let now = Instant::now();
        let later = now + Duration::from_secs(3);
        assert_eq!(future_or_floor(later, now), later, "never clamped later");
        assert_eq!(
            future_or_floor(now, now),
            now + PAST_DEADLINE_FLOOR,
            "a deadline that is due cannot spin the loop"
        );
        assert_eq!(
            future_or_floor(now - Duration::from_secs(5), now),
            now + PAST_DEADLINE_FLOOR
        );
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
        let serviced = tick
            .find("Self::tick_cpu_presenter(&mut ws, now)")
            .expect("about_to_wait services each CPU window's presenter");
        let armed = tick[serviced..]
            .find("ControlFlow::WaitUntil(deadline)")
            .expect("the presenter's wake reaches the control flow");
        assert!(
            tick[serviced..serviced + armed].contains("earliest_deadline"),
            "the presenter's deadline is merged into the loop's wait"
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
