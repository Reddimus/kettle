//! Startup phase stamps.
//!
//! Each phase records the raw monotonic clock the first time it is marked:
//! `CLOCK_UPTIME_RAW` on macOS, the clock the macOS standing harness's launch
//! probe and `stamp` read, and `CLOCK_MONOTONIC` on Linux. Marking is one
//! atomic store, with no allocation or I/O, so it stays on in release builds.
//! The stamps are printed once, after the first frame, only when the
//! `kettle::startup` log target is enabled at info
//! (`RUST_LOG=warn,kettle::startup=info`), which leaves every other target at
//! its usual level instead of flooding the log and slowing startup down.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// The log target the stamps print under.
pub const TARGET: &str = "kettle::startup";

/// Startup phases, listed roughly in the order they happen. The printed
/// lines are sorted by time instead, so the font thread's phases, which run
/// alongside the main thread's, can print earlier than they are listed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The first statement of `main`.
    Main,
    /// `App::run_with` starts.
    RunWith,
    /// winit's event loop is built.
    EventLoopBuilt,
    /// The config is loaded and the command-line overrides applied.
    ConfigLoaded,
    /// The `App` exists, just before the event loop runs.
    AppBuilt,
    /// The font preload thread enumerated the system fonts.
    FontsEnumerated,
    /// The font preload thread loaded the configured family and finished.
    FontsReady,
    /// The first pane's shell was spawned.
    PaneSpawned,
    /// winit delivered `Resumed` and the first window's setup begins.
    Resumed,
    /// The first window starts waiting for the font preload thread.
    FontsJoinStart,
    /// The first window has measured the fonts from the preload thread or,
    /// when the thread did not start or finish, its own fallback load.
    FontsJoined,
    /// The native window exists.
    WindowCreated,
    /// The GPU device, surface and pipelines are ready.
    GpuReady,
    /// The window was made visible.
    WindowRevealed,
    /// The first frame was drawn.
    FirstFrame,
}

impl Phase {
    pub const ALL: [Phase; 15] = [
        Phase::Main,
        Phase::RunWith,
        Phase::EventLoopBuilt,
        Phase::ConfigLoaded,
        Phase::AppBuilt,
        Phase::FontsEnumerated,
        Phase::FontsReady,
        Phase::PaneSpawned,
        Phase::Resumed,
        Phase::FontsJoinStart,
        Phase::FontsJoined,
        Phase::WindowCreated,
        Phase::GpuReady,
        Phase::WindowRevealed,
        Phase::FirstFrame,
    ];
    pub const COUNT: usize = Self::ALL.len();

    pub fn name(self) -> &'static str {
        match self {
            Phase::Main => "main",
            Phase::RunWith => "run_with",
            Phase::EventLoopBuilt => "event_loop_built",
            Phase::ConfigLoaded => "config_loaded",
            Phase::AppBuilt => "app_built",
            Phase::FontsEnumerated => "fonts_enumerated",
            Phase::FontsReady => "fonts_ready",
            Phase::PaneSpawned => "pane_spawned",
            Phase::Resumed => "resumed",
            Phase::FontsJoinStart => "fonts_join_start",
            Phase::FontsJoined => "fonts_joined",
            Phase::WindowCreated => "window_created",
            Phase::GpuReady => "gpu_ready",
            Phase::WindowRevealed => "window_revealed",
            Phase::FirstFrame => "first_frame",
        }
    }

    /// The thread that marks this phase.
    fn thread(self) -> &'static str {
        match self {
            Phase::FontsEnumerated | Phase::FontsReady => "fonts",
            _ => "main",
        }
    }
}

/// How the first pane's shell was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StartupPath {
    /// Spawned in `resumed` before the window, from the monitor's size.
    ResumedEarly = 1,
    /// Spawned after the renderer, once the window's size was known.
    AfterRenderer = 2,
}

impl StartupPath {
    fn name(self) -> &'static str {
        match self {
            StartupPath::ResumedEarly => "resumed_early",
            StartupPath::AfterRenderer => "after_renderer",
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(StartupPath::ResumedEarly),
            2 => Some(StartupPath::AfterRenderer),
            _ => None,
        }
    }
}

static STAMPS: [AtomicU64; Phase::COUNT] = [const { AtomicU64::new(0) }; Phase::COUNT];
static PATH: AtomicU8 = AtomicU8::new(0);

/// The raw clock, in nanoseconds.
pub fn now_ns() -> u64 {
    #[cfg(unix)]
    {
        #[cfg(target_os = "macos")]
        const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
        #[cfg(not(target_os = "macos"))]
        const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `now` is a valid timespec for the call to fill, and the
        // clock id exists on this platform.
        unsafe { libc::clock_gettime(CLOCK, &mut now) };
        now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
    }
    #[cfg(not(unix))]
    {
        // No external harness reads these times on Windows; they only need to
        // be monotonic relative to one another.
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos() as u64
            + 1
    }
}

/// Record `phase` now, unless it was already recorded.
pub fn mark(phase: Phase) {
    let slot = &STAMPS[phase as usize];
    // Later windows reach the same marks; skip the clock read for them.
    if slot.load(Ordering::Relaxed) != 0 {
        return;
    }
    let _ = slot.compare_exchange(0, now_ns().max(1), Ordering::Relaxed, Ordering::Relaxed);
}

/// Record how the first pane started, unless already recorded.
pub fn note_path(path: StartupPath) {
    let _ = PATH.compare_exchange(0, path as u8, Ordering::Relaxed, Ordering::Relaxed);
}

/// The printed lines for a set of stamps, in the order the phases happened;
/// unmarked phases are left out.
fn render(stamps: &[u64; Phase::COUNT], path: Option<StartupPath>) -> Vec<String> {
    let main = stamps[Phase::Main as usize];
    let mut marked: Vec<&Phase> = Phase::ALL
        .iter()
        .filter(|phase| stamps[**phase as usize] != 0)
        .collect();
    // Stable, so phases stamped in the same nanosecond keep their listed order.
    marked.sort_by_key(|phase| stamps[**phase as usize]);
    let mut lines: Vec<String> = marked
        .into_iter()
        .map(|phase| {
            let t = stamps[*phase as usize];
            let since_main = if main != 0 && t >= main {
                format!("{:.2}", (t - main) as f64 / 1e6)
            } else {
                "-".to_owned()
            };
            format!(
                "startup phase={} t_ns={t} since_main_ms={since_main} thread={}",
                phase.name(),
                phase.thread()
            )
        })
        .collect();
    lines.push(format!(
        "startup path={}",
        path.map_or("unknown", StartupPath::name)
    ));
    lines
}

/// Print the stamps once, if the `kettle::startup` target is enabled.
pub fn flush() {
    static FLUSHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !log::log_enabled!(target: TARGET, log::Level::Info) || FLUSHED.swap(true, Ordering::Relaxed)
    {
        return;
    }
    let stamps: [u64; Phase::COUNT] = std::array::from_fn(|i| STAMPS[i].load(Ordering::Relaxed));
    for line in render(&stamps, StartupPath::from_u8(PATH.load(Ordering::Relaxed))) {
        log::info!(target: TARGET, "{line}");
    }
}

/// Flushes the trace when dropped. `run_with` holds one, so a startup that
/// returns before its first frame, from any point, still reports how far it
/// got; after a first frame the flush does nothing.
pub struct FlushOnDrop;

impl Drop for FlushOnDrop {
    fn drop(&mut self) {
        flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_mark_wins_and_the_clock_moves_forward() {
        let first = now_ns();
        let slot = AtomicU64::new(0);
        let _ = slot.compare_exchange(0, first.max(1), Ordering::Relaxed, Ordering::Relaxed);
        let _ = slot.compare_exchange(0, first + 5, Ordering::Relaxed, Ordering::Relaxed);
        assert_eq!(slot.load(Ordering::Relaxed), first.max(1));
        assert!(now_ns() >= first);
        // The real table keeps its first value too.
        mark(Phase::FirstFrame);
        let kept = STAMPS[Phase::FirstFrame as usize].load(Ordering::Relaxed);
        mark(Phase::FirstFrame);
        assert_eq!(
            STAMPS[Phase::FirstFrame as usize].load(Ordering::Relaxed),
            kept
        );
        assert_ne!(kept, 0);
    }

    #[test]
    fn phases_print_in_the_order_they_happened() {
        let mut stamps = [0u64; Phase::COUNT];
        stamps[Phase::Main as usize] = 1_000;
        stamps[Phase::PaneSpawned as usize] = 3_000;
        stamps[Phase::Resumed as usize] = 2_000;
        let names: Vec<String> = render(&stamps, None)
            .iter()
            .filter_map(|line| line.split_whitespace().nth(1).map(str::to_owned))
            .collect();
        assert_eq!(
            names,
            [
                "phase=main",
                "phase=resumed",
                "phase=pane_spawned",
                "path=unknown"
            ]
        );
    }

    #[test]
    fn unmarked_phases_are_left_out() {
        let mut stamps = [0u64; Phase::COUNT];
        stamps[Phase::Main as usize] = 1_000_000;
        stamps[Phase::FirstFrame as usize] = 251_000_000;
        let lines = render(&stamps, None);
        assert_eq!(
            lines,
            [
                "startup phase=main t_ns=1000000 since_main_ms=0.00 thread=main",
                "startup phase=first_frame t_ns=251000000 since_main_ms=250.00 thread=main",
                "startup path=unknown",
            ]
        );
    }

    /// The harness parses the same fixture (macos-standing-self-test.py), so
    /// the format cannot drift on one side only.
    #[test]
    fn the_printed_lines_match_the_harness_fixture() {
        let fixture = include_str!("../../../scripts/perf/macos-standing/startup-phases.fixture");
        let mut stamps = [0u64; Phase::COUNT];
        for (i, phase) in Phase::ALL.iter().enumerate() {
            stamps[*phase as usize] = 1_000_000_000 + i as u64 * 10_000_000;
        }
        let expected: Vec<&str> = fixture
            .lines()
            .filter_map(|line| line.split_once("kettle::startup: ").map(|(_, rest)| rest))
            .collect();
        assert_eq!(render(&stamps, Some(StartupPath::ResumedEarly)), expected);
    }

    /// Every phase is marked in production code, only the phases with more
    /// than one site (a reveal per platform path, a spawn per startup path)
    /// are marked more than once, and the marks appear in startup order.
    #[test]
    fn the_guard_flushes_when_dropped() {
        // The production half only: this test's own text holds the needle.
        let text = include_str!("startup_trace.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        let body = text
            .split_once("impl Drop for FlushOnDrop {")
            .expect("FlushOnDrop has a Drop impl")
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        assert!(body.contains("flush();"));
    }

    #[test]
    fn every_phase_is_marked_in_startup_order() {
        let main_rs = include_str!("../../kettle/src/main.rs");
        let app_rs = include_str!("app.rs");
        let production = app_rs.split("#[cfg(test)]\nmod tests").next().unwrap();
        let resumed = production.split_once("fn resumed_inner(").unwrap().1;
        let resumed = resumed.split_once("\n    fn ").unwrap().0;
        let run_with = production.split_once("pub fn run_with(").unwrap().1;
        let run_with = run_with.split_once("\n    fn ").unwrap().0;
        // Every call spells the full path, so this matches `crate::` and
        // `kettle_ui::` calls alike and nothing else.
        let needle =
            |phase: Phase| format!("startup_trace::mark(crate::startup_trace::Phase::{phase:?})");
        // Compare without whitespace or trailing commas, so rustfmt's line
        // breaks never matter.
        let flat = |text: &str| -> String {
            text.split_whitespace()
                .collect::<String>()
                .replace(",)", ")")
        };
        let count = |text: &str, phase: Phase| flat(text).matches(&needle(phase)).count();

        let main_mark = "kettle_ui::startup_trace::mark(kettle_ui::startup_trace::Phase::Main);";
        assert_eq!(main_rs.matches(main_mark).count(), 1);
        let main_body = main_rs
            .split_once("fn main() -> anyhow::Result<()> {")
            .unwrap()
            .1;
        assert!(
            main_body.trim_start().starts_with(main_mark),
            "main marks its phase before anything else"
        );
        for phase in [
            Phase::RunWith,
            Phase::EventLoopBuilt,
            Phase::ConfigLoaded,
            Phase::AppBuilt,
        ] {
            assert_eq!(count(production, phase), 1, "{phase:?}");
            assert_eq!(count(run_with, phase), 1, "{phase:?} is marked in run_with");
        }
        for phase in [Phase::Resumed, Phase::WindowCreated, Phase::GpuReady] {
            assert_eq!(count(production, phase), 1, "{phase:?}");
            assert_eq!(
                count(resumed, phase),
                1,
                "{phase:?} is marked in resumed_inner"
            );
        }
        assert_eq!(
            count(resumed, Phase::PaneSpawned),
            2,
            "the early and the after-renderer spawn"
        );
        assert!(count(production, Phase::WindowRevealed) >= 2);

        let at = |text: &str, pattern: &str| {
            flat(text)
                .find(&flat(pattern))
                .unwrap_or_else(|| panic!("missing {pattern}"))
        };
        let order = [
            at(run_with, &needle(Phase::RunWith)),
            at(
                run_with,
                "EventLoop::<UserEvent>::with_user_event().build()",
            ),
            at(run_with, &needle(Phase::EventLoopBuilt)),
            at(run_with, &needle(Phase::ConfigLoaded)),
            at(run_with, &needle(Phase::AppBuilt)),
            at(run_with, "event_loop.run_app(&mut app)"),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "run_with marks out of order: {order:?}"
        );
        // A window that starts hidden ends its trace in resumed_inner.
        let hidden_gate = at(
            resumed,
            "if !should_reveal_after_renderer_init(self.cfg.window_state) {",
        );
        let order = [
            at(resumed, &needle(Phase::Resumed)),
            at(resumed, "event_loop.create_window(attrs)"),
            at(resumed, &needle(Phase::WindowCreated)),
            at(resumed, &needle(Phase::GpuReady)),
            at(resumed, "self.redraw(ws);"),
            hidden_gate,
            hidden_gate + at(&flat(resumed)[hidden_gate..], "startup_trace::flush()"),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "resumed_inner marks out of order: {order:?}"
        );

        // The font preload marks its own phases: two on its thread, and the
        // first window's wait for it in `finish`, which resumed_inner calls
        // before it spawns the first pane.
        let preload = include_str!("font_preload.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        for phase in [
            Phase::FontsEnumerated,
            Phase::FontsReady,
            Phase::FontsJoinStart,
            Phase::FontsJoined,
        ] {
            assert_eq!(count(preload, phase), 1, "{phase:?} in font_preload.rs");
            assert_eq!(count(production, phase), 0, "{phase:?} in app.rs");
        }
        // The thread warms the compiled-in family while the config is still
        // being read, not after it arrives, so a join right after the config
        // does not wait for that warm-up.
        let thread = preload.split_once("fn prepare(").unwrap().1;
        let order = [
            at(thread, "raise_priority();"),
            at(thread, "PreparedFonts::enumerate()"),
            at(thread, &needle(Phase::FontsEnumerated)),
            at(thread, "prepared.warm_family(kettle_config::font::FAMILY);"),
            at(thread, "requested.recv()"),
            at(thread, "prepared.warm_family(&family);"),
            at(thread, &needle(Phase::FontsReady)),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "the preload thread marks out of order: {order:?}"
        );
        // A fallback load happens inside the stamped wait, so the trace never
        // shows a short wait while the first window loaded the fonts itself.
        let finish = preload.split_once("fn finish(").unwrap().1;
        let finish = finish.split_once("\n    }\n").unwrap().0;
        let order = [
            at(finish, &needle(Phase::FontsJoinStart)),
            at(finish, "thread.join()"),
            at(finish, "unwrap_or_else(PreparedFonts::enumerate)"),
            at(finish, "prepared.measure(cfg, scale)"),
            at(finish, &needle(Phase::FontsJoined)),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "finish marks out of order: {order:?}"
        );
        let order = [
            at(resumed, &needle(Phase::Resumed)),
            at(resumed, "preload.finish(&self.cfg, startup_scale)"),
            at(resumed, "self.spawn_first_tab(ws, has_launch_override)"),
            at(resumed, "event_loop.create_window(attrs)"),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "resumed_inner joins the font preload out of order: {order:?}"
        );

        // The first frame is the first one presented, marked after the
        // fallback reveal so the trace it prints includes that reveal.
        let redraw = production
            .split_once("fn redraw(&mut self, ws: &mut WindowState) {")
            .unwrap()
            .1;
        let presented = redraw
            .split_once("FrameOutcome::Presented => {")
            .unwrap()
            .1
            .split_once("FrameOutcome::RetryLater =>")
            .unwrap()
            .0;
        assert_eq!(count(production, Phase::FirstFrame), 1);
        assert_eq!(count(presented, Phase::FirstFrame), 1);
        // Only the window startup created: a restored secondary window can
        // present before the first window's pane has spawned.
        let startup_only = "if ws.startup_window {";
        assert_eq!(flat(presented).matches(&flat(startup_only)).count(), 2);
        let first_gate = at(presented, startup_only);
        let last_gate = flat(presented).rfind(&flat(startup_only)).unwrap();
        let order = [
            first_gate,
            at(presented, &needle(Phase::WindowRevealed)),
            last_gate,
            at(presented, &needle(Phase::FirstFrame)),
            at(presented, "startup_trace::flush()"),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "the presented frame marks out of order or ungated: {order:?}"
        );
        assert_eq!(flat(resumed).matches("ws.startup_window=true;").count(), 1);
        assert_eq!(flat(production).matches("startup_window=true").count(), 1);
        // Every other window starts without the flag.
        let window_state_rs = flat(include_str!("window_state.rs"));
        assert_eq!(window_state_rs.matches("startup_window:false,").count(), 1);
        assert!(!window_state_rs.contains("startup_window:true"));
        assert!(!window_state_rs.contains("startup_window=true"));

        // The one-shot flush runs only where this guard pins it: the startup
        // window's first frame, a hidden startup's end, and the watchdog's
        // exit; plus run_with's guard, which nothing may drop early.
        assert_eq!(
            flat(production).matches("startup_trace::flush()").count(),
            3
        );
        assert!(!flat(main_rs).contains("startup_trace::flush"));
        assert_eq!(
            flat(production)
                .matches("startup_trace::FlushOnDrop")
                .count(),
            1
        );
        assert!(!flat(run_with).contains("drop(_trace)"));

        // A startup that ends before any frame still prints its trace: every
        // return from run_with, early or not, drops the guard, and the GPU
        // watchdog's exit, which skips destructors, flushes first.
        assert!(
            at(run_with, "let _trace = crate::startup_trace::FlushOnDrop;")
                < at(
                    run_with,
                    "EventLoop::<UserEvent>::with_user_event().build()"
                ),
            "run_with holds the flush guard before anything can fail"
        );
        let watchdog = resumed
            .split_once("GPU initialization did not finish")
            .unwrap()
            .1;
        assert!(
            at(watchdog, "startup_trace::flush();") < at(watchdog, "std::process::exit(1);"),
            "the GPU watchdog flushes before it exits"
        );
    }
}
