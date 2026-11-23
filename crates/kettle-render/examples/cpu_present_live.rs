//! Live check of `kettle_render::cpu_present` on a real window (Linux).
//!
//! Opens a winit window and, with no GPU code, presents frames through the
//! CPU presenter: a moving square with exact damage, opaque and translucent
//! frames in turn, resizes, a flood of presents faster than the compositor's
//! frame clock, a forced second buffer, idle release and occlusion. Each
//! frame repaints only the region the presenter asks for and is then checked
//! against a full reference frame, so a wrong buffer age shows up as a
//! mismatch. On Wayland it then checks three cases the protocol tests cannot
//! reach, because they turn on winit's loop or the compositor:
//!
//! - **Burst.** Twenty bursts of presents, each ending on a deferred frame,
//!   with nothing else to wake the loop. The deferred frame must go out on
//!   the presenter's own deadline, and its frame callback must come back.
//!   `--burst-wait` waits under `ControlFlow::Wait` instead, without the
//!   deadline, to show the frame staying off screen.
//! - **Abandon.** A frame drawn over a deferred frame's buffer and dropped
//!   half-way must never be committed.
//! - **Hidden.** A minimized window keeps presenting; the compositor
//!   withholds frame callbacks, and the presenter must commit on its
//!   timeout without asking for more callbacks.
//!
//! On X11 it reads a pixel of an opaque frame back from the server: an ARGB
//! window must get alpha 0xff.
//!
//! With `CPU_PRESENT_SHOT_DIR` set, it asks the harness for a screenshot at
//! the end of the burst and abandon checks by writing `<tag>.req` (a JSON
//! line: the colour that must be on screen and colours that must not), and
//! waits for `<tag>.done`. It prints one `CPU_PRESENT_RESULT {json}` line and
//! exits 1 if any check failed.
//!
//! ```sh
//! cargo run -p kettle-render --example cpu_present_live [-- --softbuffer] [--size WxH] [--maximize] [--burst-wait]
//! ```
//!
//! Wayland is used when `WAYLAND_DISPLAY` is set; unset it and set
//! `DISPLAY` for X11.

#[cfg(target_os = "linux")]
fn main() {
    live::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cpu_present_live runs on Linux only");
}

#[cfg(target_os = "linux")]
mod live {
    use std::fmt::Write as _;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use kettle_render::cpu_present::{
        CpuPresenter, FrameError, PixelFormat, PresentOutcome, PresenterPreference, Rect,
    };
    use winit::application::ApplicationHandler;
    use winit::dpi::{LogicalSize, PhysicalSize};
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
    use winit::window::{Window, WindowId};

    const SQUARE: u32 = 40;
    const BURST_ROUNDS: u32 = 20;
    /// How long the minimized window keeps drawing.
    const HIDDEN_MS: u64 = 4500;
    /// What the abandoned frame writes: it must never reach the screen.
    const GARBAGE: u32 = 0xffff_00ff;
    const RESIZES: [(u32, u32); 4] = [(900, 600), (640, 400), (1000, 640), (0, 0)];

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Phase {
        First,
        Damage,
        Formats,
        Resize(usize),
        Flood,
        TwoBuffers,
        Idle,
        Occluded,
        Burst,
        Abandon,
        Hidden,
        Done,
    }

    #[derive(Default)]
    struct Report {
        kind: String,
        t_window_us: u128,
        t_presenter_us: u128,
        t_first_present_us: u128,
        t_first_callback_us: u128,
        presenter_init_us: u128,
        frames: u64,
        checked: u64,
        mismatched_frames: u64,
        presented: u64,
        deferred: u64,
        unchanged: u64,
        busy: u64,
        partial_frames: u64,
        format_frames: [u64; 2],
        translucency: bool,
        resizes_seen: usize,
        resize_failures: Vec<String>,
        flood_presents: u64,
        flood_commits: u64,
        flood_max_buffers: usize,
        two_buffers: usize,
        pss_two_kib: u64,
        shm_two_kib: u64,
        idle_buffers_before_release: usize,
        released_at_idle: usize,
        idle_buffers: usize,
        pss_one_kib: u64,
        shm_one_kib: u64,
        occluded_buffers: usize,
        pss_zero_kib: u64,
        shm_zero_kib: u64,
        unoccluded_age: u8,
        unoccluded_outcome: String,
        allocations: u64,
        burst_mode: String,
        burst_rounds: u32,
        burst_committed: u32,
        burst_on_screen: u32,
        burst_stalled: u32,
        burst_latency_us: Vec<u128>,
        abandon_deferred_after_drop: bool,
        abandon_flush: String,
        abandon_next_age: u8,
        abandon_next_outcome: String,
        hidden_presents: u64,
        hidden_commits: u64,
        hidden_timed_out: u64,
        hidden_callback_pending: bool,
        x11_alpha: Option<u8>,
        shots: Vec<String>,
        failures: Vec<String>,
    }

    struct App {
        start: Instant,
        preference: PresenterPreference,
        initial: (u32, u32),
        maximize: bool,
        window: Option<Arc<Window>>,
        presenter: Option<CpuPresenter>,
        size: (u32, u32),
        scale: f64,
        format: PixelFormat,
        truth: Vec<u32>,
        square: (u32, u32),
        frame_no: u32,
        phase: Phase,
        phase_frames: u32,
        phase_started: Instant,
        wake_at: Option<Instant>,
        awaiting_first_callback: bool,
        resize_target: Option<(u32, u32)>,
        last_age: u8,
        /// The frame number of the last committed frame.
        committed_frame: u32,
        /// That number before a burst's deferred frame went out.
        burst_committed_before: u32,
        /// The phase after the screenshot asked for.
        after_shot: Phase,
        proxy: EventLoopProxy<()>,
        burst_wait: bool,
        /// The deferred frame ending a burst: when the burst ended.
        burst_end: Option<Instant>,
        /// A commit whose frame callback is awaited, and since when.
        settling: Option<Instant>,
        /// Abandon: 1 after the dropped draw, 2 after the next frame.
        abandon_step: u8,
        hidden_until: Option<Instant>,
        hidden_base: (u64, u64),
        /// A screenshot asked for: its tag and when to give up.
        shot: Option<(String, Instant)>,
        report: Report,
    }

    pub fn main() {
        let preference = if std::env::args().any(|a| a == "--softbuffer") {
            PresenterPreference::Softbuffer
        } else {
            PresenterPreference::Auto
        };
        // `--size WxH` sets the first window size (logical pixels).
        let initial = std::env::args()
            .skip_while(|a| a != "--size")
            .nth(1)
            .and_then(|v| {
                let (w, h) = v.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((800, 500));
        let start = Instant::now();
        // The event loop can wait forever when no event comes; bound the run.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(90));
            eprintln!("cpu_present_live: watchdog fired");
            std::process::exit(2);
        });
        let event_loop = EventLoop::with_user_event().build().expect("event loop");
        let burst_wait = std::env::args().any(|a| a == "--burst-wait");
        let mut app = App {
            start,
            preference,
            initial,
            maximize: std::env::args().any(|a| a == "--maximize"),
            window: None,
            presenter: None,
            size: (1, 1),
            scale: 1.0,
            format: PixelFormat::Opaque,
            truth: Vec::new(),
            square: (0, 0),
            frame_no: 0,
            phase: Phase::First,
            phase_frames: 0,
            phase_started: start,
            wake_at: None,
            awaiting_first_callback: false,
            resize_target: None,
            last_age: 0,
            committed_frame: 0,
            burst_committed_before: 0,
            after_shot: Phase::Done,
            proxy: event_loop.create_proxy(),
            burst_wait,
            burst_end: None,
            settling: None,
            abandon_step: 0,
            hidden_until: None,
            hidden_base: (0, 0),
            shot: None,
            report: Report {
                burst_mode: if burst_wait { "wait" } else { "deadline" }.to_string(),
                ..Report::default()
            },
        };
        event_loop.run_app(&mut app).expect("run");
        let ok = app.report.failures.is_empty()
            && app.report.mismatched_frames == 0
            && app.report.resize_failures.is_empty();
        println!("CPU_PRESENT_RESULT {}", app.report.json(ok));
        std::process::exit(if ok { 0 } else { 1 });
    }

    fn background(format: PixelFormat) -> u32 {
        match format {
            // The alpha byte of an opaque frame is the presenter's to set:
            // leave it 0, as a renderer may.
            PixelFormat::Opaque => 0x0020_3040,
            // 75% alpha, premultiplied: each channel scaled by 0xc0 / 0xff.
            PixelFormat::Premultiplied => {
                let a = 0xc0u32;
                let pm = |c: u32| (c * a + 127) / 255;
                (a << 24) | (pm(0x20) << 16) | (pm(0x30) << 8) | pm(0x40)
            }
        }
    }

    fn square_colour(frame: u32) -> u32 {
        0xff00_0000 | ((frame * 37) & 0xff) << 16 | 0xc0 << 8 | ((frame * 11) & 0xff)
    }

    fn fill(buf: &mut [u32], width: u32, rect: Rect, value: u32) {
        for y in rect.y..rect.y + rect.height {
            let row = (y * width) as usize;
            buf[row + rect.x as usize..row + (rect.x + rect.width) as usize].fill(value);
        }
    }

    fn copy_rect(dst: &mut [u32], src: &[u32], width: u32, rect: Rect) {
        for y in rect.y..rect.y + rect.height {
            let a = (y * width + rect.x) as usize;
            let b = a + rect.width as usize;
            dst[a..b].copy_from_slice(&src[a..b]);
        }
    }

    /// `(Pss of the process, Pss of the presenter's shared memory)` in KiB.
    /// With `CPU_PRESENT_SMAPS_DIR` set, keep a copy of `/proc/self/smaps`
    /// for each sample.
    fn keep_smaps(tag: &str) {
        if let Some(dir) = std::env::var_os("CPU_PRESENT_SMAPS_DIR") {
            let path = std::path::Path::new(&dir).join(format!("{tag}.smaps"));
            if let Ok(text) = std::fs::read_to_string("/proc/self/smaps") {
                let _ = std::fs::write(path, text);
            }
        }
    }

    fn pss() -> (u64, u64) {
        let total = std::fs::read_to_string("/proc/self/smaps_rollup")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Pss:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
            })
            .unwrap_or(0);
        let mut shm = 0;
        let mut in_shm = false;
        if let Ok(smaps) = std::fs::read_to_string("/proc/self/smaps") {
            for line in smaps.lines() {
                let first = line.split_whitespace().next().unwrap_or("");
                if first.contains('-')
                    && first.chars().next().is_some_and(|c| c.is_ascii_hexdigit())
                {
                    // Kettle's wl_shm memfds, softbuffer's (Wayland), and
                    // System V segments (MIT-SHM on X11).
                    in_shm = line.contains("memfd:kettle-cpu-present")
                        || line.contains("memfd:softbuffer")
                        || line.contains("/SYSV");
                } else if in_shm && line.starts_with("Pss:") {
                    shm += line
                        .split_whitespace()
                        .nth(1)
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0);
                }
            }
        }
        (total, shm)
    }

    /// Ask the harness for a screenshot: `colour` must be on screen (a whole
    /// square of it), `absent` colours must not be anywhere. `false` without
    /// `CPU_PRESENT_SHOT_DIR`.
    fn request_shot(tag: &str, colour: u32, absent: &[u32]) -> bool {
        let Some(dir) = std::env::var_os("CPU_PRESENT_SHOT_DIR") else {
            return false;
        };
        let hex = |c: u32| format!("\"{:06x}\"", c & 0x00ff_ffff);
        let absent: Vec<String> = absent.iter().map(|&c| hex(c)).collect();
        let line = format!(
            "{{\"expect\":{},\"pixels\":{},\"absent\":[{}]}}\n",
            hex(colour),
            SQUARE * SQUARE,
            absent.join(",")
        );
        std::fs::write(std::path::Path::new(&dir).join(format!("{tag}.req")), line).is_ok()
    }

    fn shot_done(tag: &str) -> bool {
        std::env::var_os("CPU_PRESENT_SHOT_DIR").is_some_and(|dir| {
            std::path::Path::new(&dir)
                .join(format!("{tag}.done"))
                .exists()
        })
    }

    /// The alpha byte of the window's bottom-right pixel as the X server
    /// holds it, when the window has a depth-32 visual. The square never
    /// reaches that pixel. The example's own connection, to `$DISPLAY`,
    /// which is the window's server here.
    fn x11_alpha(window: &Window, w: u32, h: u32) -> Option<u8> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
        let id = match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok()?,
            RawWindowHandle::Xcb(h) => h.window.get(),
            _ => return None,
        };
        let (conn, _) = x11rb::connect(None).ok()?;
        let x = i16::try_from(w.saturating_sub(1)).ok()?;
        let y = i16::try_from(h.saturating_sub(1)).ok()?;
        let image = conn
            .get_image(ImageFormat::Z_PIXMAP, id, x, y, 1, 1, !0)
            .ok()?
            .reply()
            .ok()?;
        (image.depth == 32)
            .then(|| image.data.get(3).copied())
            .flatten()
    }

    impl Report {
        fn fail(&mut self, msg: String) {
            eprintln!("cpu_present_live: FAIL {msg}");
            self.failures.push(msg);
        }

        fn json(&self, ok: bool) -> String {
            let list = |v: &[String]| {
                let items: Vec<String> = v.iter().map(|s| format!("{s:?}")).collect();
                format!("[{}]", items.join(","))
            };
            let mut s = String::from("{");
            let _ = write!(
                s,
                "\"ok\":{ok},\"kind\":{:?},\"t_window_us\":{},\"t_presenter_us\":{},\
                 \"presenter_init_us\":{},\"t_first_present_us\":{},\"t_first_callback_us\":{},\
                 \"frames\":{},\"checked\":{},\"mismatched_frames\":{},\"presented\":{},\
                 \"deferred\":{},\"unchanged\":{},\"busy\":{},\"partial_frames\":{},\
                 \"opaque_frames\":{},\"translucent_frames\":{},\"translucency\":{},\
                 \"resizes_seen\":{},\"resize_failures\":{},\"flood_presents\":{},\
                 \"flood_commits\":{},\"flood_max_buffers\":{},\"two_buffers\":{},\
                 \"pss_two_kib\":{},\"shm_two_kib\":{},\"idle_buffers_before_release\":{},\
                 \"released_at_idle\":{},\"idle_buffers\":{},\"pss_one_kib\":{},\
                 \"shm_one_kib\":{},\"occluded_buffers\":{},\"pss_zero_kib\":{},\
                 \"shm_zero_kib\":{},\"unoccluded_age\":{},\"unoccluded_outcome\":{:?},\
                 \"allocations\":{},",
                self.kind,
                self.t_window_us,
                self.t_presenter_us,
                self.presenter_init_us,
                self.t_first_present_us,
                self.t_first_callback_us,
                self.frames,
                self.checked,
                self.mismatched_frames,
                self.presented,
                self.deferred,
                self.unchanged,
                self.busy,
                self.partial_frames,
                self.format_frames[0],
                self.format_frames[1],
                self.translucency,
                self.resizes_seen,
                list(&self.resize_failures),
                self.flood_presents,
                self.flood_commits,
                self.flood_max_buffers,
                self.two_buffers,
                self.pss_two_kib,
                self.shm_two_kib,
                self.idle_buffers_before_release,
                self.released_at_idle,
                self.idle_buffers,
                self.pss_one_kib,
                self.shm_one_kib,
                self.occluded_buffers,
                self.pss_zero_kib,
                self.shm_zero_kib,
                self.unoccluded_age,
                self.unoccluded_outcome,
                self.allocations,
            );
            let lat = {
                let mut v = self.burst_latency_us.clone();
                v.sort_unstable();
                let median = v.get(v.len() / 2).copied().unwrap_or(0);
                (median, v.last().copied().unwrap_or(0))
            };
            let _ = write!(
                s,
                "\"burst_mode\":{:?},\"burst_rounds\":{},\"burst_committed\":{},\
                 \"burst_on_screen\":{},\"burst_stalled\":{},\"burst_latency_median_us\":{},\
                 \"burst_latency_max_us\":{},\"abandon_deferred_after_drop\":{},\
                 \"abandon_flush\":{:?},\"abandon_next_age\":{},\"abandon_next_outcome\":{:?},\
                 \"hidden_presents\":{},\"hidden_commits\":{},\"hidden_timed_out\":{},\
                 \"hidden_callback_pending\":{},\"x11_alpha\":{},\"shots\":{},\"failures\":{}",
                self.burst_mode,
                self.burst_rounds,
                self.burst_committed,
                self.burst_on_screen,
                self.burst_stalled,
                lat.0,
                lat.1,
                self.abandon_deferred_after_drop,
                self.abandon_flush,
                self.abandon_next_age,
                self.abandon_next_outcome,
                self.hidden_presents,
                self.hidden_commits,
                self.hidden_timed_out,
                self.hidden_callback_pending,
                self.x11_alpha.map_or("null".to_string(), |a| a.to_string()),
                list(&self.shots),
                list(&self.failures),
            );
            s.push('}');
            s
        }
    }

    impl App {
        fn reset_truth(&mut self) {
            let (w, h) = self.size;
            // Reuse the allocation: freeing a large block moves glibc's mmap
            // threshold, and later frames would land on the brk heap and
            // stay there, which muddles the PSS samples.
            self.truth.clear();
            self.truth.resize((w * h) as usize, background(self.format));
            self.square = (0, 0);
            let sq = self.square_rect();
            fill(&mut self.truth, w, sq, square_colour(self.frame_no));
        }

        fn square_rect(&self) -> Rect {
            let (w, h) = self.size;
            Rect::new(self.square.0, self.square.1, SQUARE.min(w), SQUARE.min(h))
                .clipped(w, h)
                .unwrap_or(Rect::new(0, 0, 0, 0))
        }

        /// Advance the scene one step and return what changed.
        fn step(&mut self) -> Vec<Rect> {
            let (w, h) = self.size;
            let old = self.square_rect();
            fill(&mut self.truth, w, old, background(self.format));
            let span_x = w.saturating_sub(SQUARE).max(1);
            let span_y = h.saturating_sub(SQUARE).max(1);
            self.square = ((self.frame_no * 13) % span_x, (self.frame_no * 7) % span_y);
            let new = self.square_rect();
            fill(&mut self.truth, w, new, square_colour(self.frame_no));
            vec![old, new]
        }

        fn sync_size(&mut self) {
            let Some(window) = &self.window else { return };
            let PhysicalSize { width, height } = window.inner_size();
            self.scale = window.scale_factor();
            let presenter = self.presenter.as_mut().expect("presenter");
            presenter.resize(width, height, self.scale);
            let size = presenter.size();
            if size != self.size {
                self.size = size;
                self.reset_truth();
            }
        }

        fn set_format(&mut self, want: PixelFormat) {
            let presenter = self.presenter.as_mut().expect("presenter");
            let got = presenter.set_format(want);
            if let Some(window) = &self.window {
                window.set_transparent(got == PixelFormat::Premultiplied);
            }
            if got != self.format {
                self.format = got;
                self.reset_truth();
            }
        }

        /// Draw one frame: advance the scene, repaint only what the presenter
        /// asks for, check the whole buffer, present.
        fn draw(&mut self) -> Option<PresentOutcome> {
            self.frame_no += 1;
            let damage = self.step();
            let window = self.window.clone()?;
            let presenter = self.presenter.as_mut()?;
            let mut frame = match presenter.buffer_mut() {
                Ok(frame) => frame,
                Err(FrameError::Busy) => {
                    self.report.busy += 1;
                    return None;
                }
                Err(err) => {
                    self.report.fail(format!("buffer_mut: {err}"));
                    return None;
                }
            };
            let (w, h) = (frame.width(), frame.height());
            if (w, h) != self.size {
                self.report
                    .fail(format!("frame {}x{h} != scene {:?}", w, self.size));
                return None;
            }
            self.last_age = frame.age();
            let repaint = frame.repaint_region(&damage);
            let full = repaint.len() == 1 && repaint[0] == Rect::full(w, h);
            if !full {
                self.report.partial_frames += 1;
            }
            for rect in &repaint {
                copy_rect(frame.pixels_mut(), &self.truth, w, *rect);
            }
            self.report.checked += 1;
            let mask = if frame.format() == PixelFormat::Opaque {
                0x00ff_ffff
            } else {
                u32::MAX
            };
            if frame
                .pixels()
                .iter()
                .zip(&self.truth)
                .any(|(a, b)| a & mask != b & mask)
            {
                self.report.mismatched_frames += 1;
            }
            self.report.format_frames[usize::from(frame.format() == PixelFormat::Premultiplied)] +=
                1;
            let outcome = match frame.present_with(&damage, || window.pre_present_notify()) {
                Ok(o) => o,
                Err(err) => {
                    self.report.fail(format!("present: {err:#}"));
                    return None;
                }
            };
            self.report.frames += 1;
            match outcome {
                PresentOutcome::Presented => {
                    self.report.presented += 1;
                    self.committed_frame = self.frame_no;
                }
                PresentOutcome::Deferred => self.report.deferred += 1,
                PresentOutcome::Unchanged => self.report.unchanged += 1,
            }
            Some(outcome)
        }

        fn next_phase(&mut self, phase: Phase) {
            eprintln!(
                "cpu_present_live: {:?} -> {phase:?} at {} ms, {:?}",
                self.phase,
                self.start.elapsed().as_millis(),
                self.presenter.as_ref().map(|p| p.stats())
            );
            self.phase = phase;
            self.phase_frames = 0;
            self.phase_started = Instant::now();
        }

        fn redraw(&mut self) {
            let Some(window) = self.window.clone() else {
                return;
            };
            if self.awaiting_first_callback {
                // winit delivers this redraw after the first commit's frame
                // callback: the compositor has taken the first frame.
                self.awaiting_first_callback = false;
                self.report.t_first_callback_us = self.start.elapsed().as_micros();
            }
            self.phase_frames += 1;
            match self.phase {
                Phase::First => {
                    self.draw();
                    self.report.t_first_present_us = self.start.elapsed().as_micros();
                    self.awaiting_first_callback = true;
                    self.next_phase(Phase::Damage);
                    window.request_redraw();
                }
                Phase::Damage => {
                    self.draw();
                    if self.phase_frames >= 90 {
                        if self.report.kind == "SoftbufferX11" {
                            let (w, h) = self.size;
                            self.report.x11_alpha = x11_alpha(&window, w, h);
                            if self.report.x11_alpha.is_some_and(|a| a != 0xff) {
                                self.report.fail(format!(
                                    "an opaque frame reached the ARGB window with alpha {:?}",
                                    self.report.x11_alpha
                                ));
                            }
                        }
                        self.next_phase(Phase::Formats);
                    }
                    window.request_redraw();
                }
                Phase::Formats => {
                    if self.phase_frames % 15 == 1 {
                        let want = if (self.phase_frames / 15).is_multiple_of(2) {
                            PixelFormat::Premultiplied
                        } else {
                            PixelFormat::Opaque
                        };
                        self.set_format(want);
                        if want == PixelFormat::Premultiplied {
                            self.report.translucency |= self.format == want;
                        }
                    }
                    self.draw();
                    if self.phase_frames >= 60 {
                        self.set_format(PixelFormat::Opaque);
                        // A maximized window cannot be resized.
                        self.next_phase(if self.maximize {
                            Phase::Flood
                        } else {
                            Phase::Resize(0)
                        });
                    }
                    window.request_redraw();
                }
                Phase::Resize(i) => {
                    if let Some(target) = self.resize_target {
                        if self.size == target {
                            // The first frame at the new size: a full repaint
                            // of a buffer with unknown contents.
                            self.draw();
                            self.report.resizes_seen += 1;
                            self.resize_target = None;
                            if i + 1 < RESIZES.len() {
                                self.next_phase(Phase::Resize(i + 1));
                            } else {
                                self.next_phase(Phase::Flood);
                            }
                        } else if self.phase_started.elapsed() > Duration::from_secs(3) {
                            self.report
                                .resize_failures
                                .push(format!("no resize to {target:?}, at {:?}", self.size));
                            self.resize_target = None;
                            self.next_phase(if i + 1 < RESIZES.len() {
                                Phase::Resize(i + 1)
                            } else {
                                Phase::Flood
                            });
                        } else {
                            self.draw();
                        }
                    } else {
                        // The last resize returns to the first size.
                        let (w, h) = if i + 1 == RESIZES.len() {
                            self.initial
                        } else {
                            RESIZES[i]
                        };
                        let want = LogicalSize::new(w, h).to_physical::<u32>(self.scale);
                        self.resize_target = Some((want.width, want.height));
                        if let Some(now) = window.request_inner_size(LogicalSize::new(w, h)) {
                            let _ = now;
                            self.sync_size();
                        }
                        self.draw();
                    }
                    window.request_redraw();
                }
                Phase::Flood => {
                    // Five presents per redraw: far faster than the frame
                    // clock. Only the first per frame callback may commit.
                    let before = self.presenter.as_ref().map_or(0, |p| p.stats().commits);
                    for _ in 0..5 {
                        self.draw();
                        self.report.flood_presents += 1;
                    }
                    if let Some(p) = &self.presenter {
                        let stats = p.stats();
                        self.report.flood_commits += stats.commits - before;
                        self.report.flood_max_buffers =
                            self.report.flood_max_buffers.max(stats.buffers);
                    }
                    if self.phase_started.elapsed() >= Duration::from_secs(2) {
                        self.next_phase(Phase::TwoBuffers);
                    }
                    window.request_redraw();
                }
                Phase::TwoBuffers => {
                    // Commit, then draw again before the compositor's release
                    // can have been read: the second frame needs a second
                    // buffer. It is deferred (frame callback pending).
                    let presenter = self.presenter.as_mut().expect("presenter");
                    let _ = presenter.flush_with(|| window.pre_present_notify());
                    self.draw();
                    self.draw();
                    let stats = self.presenter.as_ref().expect("presenter").stats();
                    self.report.two_buffers = stats.buffers;
                    keep_smaps("two");
                    let (p, s) = pss();
                    self.report.pss_two_kib = p;
                    self.report.shm_two_kib = s;
                    self.next_phase(Phase::Idle);
                    // Let the deferred frame go out, then sit idle.
                    window.request_redraw();
                }
                Phase::Idle => {
                    if self.phase_frames == 1 {
                        let presenter = self.presenter.as_mut().expect("presenter");
                        let _ = presenter.flush_with(|| window.pre_present_notify());
                        self.wake_at = Some(Instant::now() + Duration::from_millis(1500));
                    }
                }
                Phase::Occluded => {
                    let presenter = self.presenter.as_mut().expect("presenter");
                    presenter.set_occluded(false);
                    let outcome = self.draw();
                    self.report.unoccluded_age = self.last_age;
                    self.report.unoccluded_outcome = format!("{outcome:?}");
                    if outcome != Some(PresentOutcome::Presented) {
                        self.report
                            .fail(format!("unoccluded frame was {outcome:?}"));
                    }
                    if self.report.kind == "WaylandShm" {
                        self.next_phase(Phase::Burst);
                        window.request_redraw();
                    } else {
                        self.finish();
                    }
                }
                Phase::Burst => self.burst(),
                Phase::Abandon => self.abandon(&window),
                Phase::Hidden | Phase::Done => {}
            }
        }

        fn timer(&mut self, event_loop: &ActiveEventLoop) {
            match self.phase {
                Phase::Idle => {
                    let presenter = self.presenter.as_mut().expect("presenter");
                    self.report.idle_buffers_before_release = presenter.stats().buffers;
                    self.report.released_at_idle = presenter.release_idle_buffers();
                    self.report.idle_buffers = presenter.stats().buffers;
                    keep_smaps("one");
                    let (p, s) = pss();
                    self.report.pss_one_kib = p;
                    self.report.shm_one_kib = s;
                    let presenter = self.presenter.as_mut().expect("presenter");
                    presenter.set_occluded(true);
                    self.report.occluded_buffers = presenter.stats().buffers;
                    keep_smaps("zero");
                    let (p, s) = pss();
                    self.report.pss_zero_kib = p;
                    self.report.shm_zero_kib = s;
                    self.next_phase(Phase::Occluded);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                Phase::Hidden => self.hidden_tick(),
                Phase::Done => {
                    if let Some(p) = &self.presenter {
                        self.report.allocations = p.stats().allocations;
                    }
                    event_loop.exit();
                }
                _ => {}
            }
        }

        fn finish(&mut self) {
            self.next_phase(Phase::Done);
            self.wake_at = Some(Instant::now() + Duration::from_millis(300));
        }

        /// One burst: presents until one is deferred, then nothing more. The
        /// presenter contract (flush in `about_to_wait`, wait on
        /// `pending_deadline`) must put that frame on screen.
        fn burst(&mut self) {
            if self.burst_end.is_some() || self.settling.is_some() {
                return;
            }
            let mut last = None;
            for _ in 0..20 {
                last = self.draw();
                if last == Some(PresentOutcome::Deferred) {
                    break;
                }
            }
            if last != Some(PresentOutcome::Deferred) {
                self.report.fail(format!(
                    "burst {} never deferred ({last:?})",
                    self.report.burst_rounds
                ));
                self.next_phase(Phase::Abandon);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
            self.report.burst_rounds += 1;
            self.burst_end = Some(Instant::now());
            if self.burst_wait {
                // The old contract: flush when the loop wakes, under
                // ControlFlow::Wait. A thread wakes the loop after 3 s to see
                // whether the frame went out, and asks for a screenshot
                // first, while the loop still sleeps.
                let proxy = self.proxy.clone();
                let round = self.report.burst_rounds;
                let colour = square_colour(self.frame_no);
                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    std::thread::sleep(Duration::from_millis(2500));
                    let tag = format!("burst-wait-{round}");
                    if request_shot(&tag, colour, &[]) {
                        while !shot_done(&tag) && t0.elapsed() < Duration::from_secs(12) {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                    std::thread::sleep(Duration::from_secs(3).saturating_sub(t0.elapsed()));
                    let _ = proxy.send_event(());
                });
            }
            // No request_redraw: nothing else wakes the loop.
        }

        /// Settled: the last commit's frame callback came back.
        fn settled(&mut self) {
            let window = self.window.clone();
            match self.phase {
                Phase::Burst => {
                    self.report.burst_on_screen += 1;
                    let rounds = if self.burst_wait { 3 } else { BURST_ROUNDS };
                    if self.report.burst_rounds < rounds {
                        if let Some(w) = &window {
                            w.request_redraw();
                        }
                        return;
                    }
                    let next = if self.burst_wait {
                        Phase::Done
                    } else {
                        Phase::Abandon
                    };
                    // The last burst's frame must be what the screen shows.
                    let colour = square_colour(self.frame_no);
                    let before = square_colour(self.burst_committed_before);
                    let absent: Vec<u32> = [before].into_iter().filter(|&c| c != colour).collect();
                    self.ask_shot("burst", colour, &absent, next);
                }
                Phase::Abandon => {
                    let colour = square_colour(self.frame_no);
                    self.ask_shot("abandon", colour, &[GARBAGE], Phase::Hidden);
                }
                _ => {}
            }
        }

        fn ask_shot(&mut self, tag: &str, colour: u32, absent: &[u32], next: Phase) {
            if request_shot(tag, colour, absent) {
                self.report.shots.push(tag.to_string());
                self.shot = Some((tag.to_string(), Instant::now() + Duration::from_secs(12)));
                self.after_shot = next;
            } else {
                self.enter(next);
            }
        }

        fn enter(&mut self, phase: Phase) {
            match phase {
                Phase::Abandon => {
                    self.next_phase(Phase::Abandon);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                Phase::Hidden => {
                    self.next_phase(Phase::Hidden);
                    if let Some(w) = &self.window {
                        w.set_minimized(true);
                    }
                    let stats = self.presenter.as_ref().expect("presenter").stats();
                    self.hidden_base = (stats.commits, stats.timed_out_commits);
                    let now = Instant::now();
                    // mutter keeps sending frame callbacks through its
                    // minimize animation (about 0.5 s): draw after it.
                    let start = now + Duration::from_millis(1000);
                    self.hidden_until = Some(start + Duration::from_millis(HIDDEN_MS));
                    self.wake_at = Some(start);
                }
                _ => self.finish(),
            }
        }

        /// A frame drawn over a deferred frame's buffer and dropped half-way
        /// must never be committed; the next frame repaints in full.
        fn abandon(&mut self, window: &Window) {
            match self.abandon_step {
                0 => {
                    self.draw();
                    if self.draw() != Some(PresentOutcome::Deferred) {
                        if self.phase_frames > 30 {
                            self.report
                                .fail("abandon: no frame was deferred".to_string());
                            self.enter(Phase::Hidden);
                        } else {
                            window.request_redraw();
                        }
                        return;
                    }
                    let presenter = self.presenter.as_mut().expect("presenter");
                    match presenter.buffer_mut() {
                        Ok(mut frame) => {
                            let n = (frame.width() as usize * 20).min(frame.pixels().len());
                            frame.pixels_mut()[..n].fill(GARBAGE);
                        }
                        Err(err) => self.report.fail(format!("abandon: buffer_mut: {err}")),
                    }
                    self.report.abandon_deferred_after_drop = presenter.has_deferred();
                    let out = presenter.flush_with(|| window.pre_present_notify());
                    self.report.abandon_flush = format!("{out:?}");
                    if self.report.abandon_deferred_after_drop
                        || !matches!(out, Ok(PresentOutcome::Unchanged))
                    {
                        self.report.fail(format!(
                            "abandon: the dropped draw was still deferred ({out:?})"
                        ));
                    }
                    self.abandon_step = 1;
                    window.request_redraw();
                }
                1 => {
                    let out = self.draw();
                    self.report.abandon_next_age = self.last_age;
                    self.report.abandon_next_outcome = format!("{out:?}");
                    self.abandon_step = 2;
                    self.settling = Some(Instant::now());
                }
                _ => {}
            }
        }

        /// While minimized: draw every 50 ms. The compositor withholds frame
        /// callbacks, so commits come only on the presenter's timeout.
        fn hidden_tick(&mut self) {
            self.draw();
            self.report.hidden_presents += 1;
            let now = Instant::now();
            if self.hidden_until.is_some_and(|until| now >= until) {
                let stats = self.presenter.as_ref().expect("presenter").stats();
                self.report.hidden_commits = stats.commits - self.hidden_base.0;
                self.report.hidden_timed_out = stats.timed_out_commits - self.hidden_base.1;
                self.report.hidden_callback_pending = stats.frame_callback_pending;
                // Withheld callbacks: commits only on the timeout, one a
                // second, and the callback stays out.
                let most = HIDDEN_MS / 1000 + 1;
                if self.report.hidden_timed_out < 2 || !stats.frame_callback_pending {
                    self.report.fail(format!(
                        "hidden: the compositor did not withhold frame callbacks ({} timed out)",
                        self.report.hidden_timed_out
                    ));
                }
                if self.report.hidden_timed_out > most {
                    self.report.fail(format!(
                        "hidden: {} timed-out commits in {HIDDEN_MS} ms",
                        self.report.hidden_timed_out
                    ));
                }
                self.finish();
            } else {
                self.wake_at = Some(now + Duration::from_millis(50));
            }
        }

        /// The presenter contract, run on every loop iteration: a deferred
        /// frame is flushed here.
        fn pump(&mut self) {
            let (Some(window), Some(presenter)) = (self.window.clone(), self.presenter.as_mut())
            else {
                return;
            };
            if !presenter.has_deferred() {
                return;
            }
            let before = self.committed_frame;
            match presenter.flush_with(|| window.pre_present_notify()) {
                Ok(PresentOutcome::Presented) => {
                    self.committed_frame = self.frame_no;
                    if let Some(end) = self.burst_end.take() {
                        self.report.burst_committed += 1;
                        self.report.burst_latency_us.push(end.elapsed().as_micros());
                        self.burst_committed_before = before;
                        self.settling = Some(Instant::now());
                    }
                }
                Ok(_) => {}
                Err(err) => self.report.fail(format!("flush: {err:#}")),
            }
        }
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("cpu-present-live")
                .with_inner_size(LogicalSize::new(self.initial.0, self.initial.1))
                .with_transparent(true)
                .with_maximized(self.maximize);
            let window = Arc::new(event_loop.create_window(attrs).expect("window"));
            self.report.t_window_us = self.start.elapsed().as_micros();
            let started = Instant::now();
            let presenter = match CpuPresenter::with_preference(
                window.clone(),
                event_loop.owned_display_handle(),
                self.preference,
            ) {
                Ok(p) => p,
                Err(err) => {
                    self.report.fail(format!("new: {err:#}"));
                    event_loop.exit();
                    return;
                }
            };
            self.report.presenter_init_us = started.elapsed().as_micros();
            self.report.t_presenter_us = self.start.elapsed().as_micros();
            self.report.kind = format!("{:?}", presenter.kind());
            self.presenter = Some(presenter);
            self.window = Some(window.clone());
            self.set_format(PixelFormat::Opaque);
            window.set_transparent(false);
            self.sync_size();
            eprintln!(
                "cpu_present_live: {} presenter ready at {} us, size {:?}",
                self.report.kind, self.report.t_presenter_us, self.size
            );
            window.request_redraw();
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    if self.presenter.is_some() {
                        self.sync_size();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
                WindowEvent::Occluded(occluded) => {
                    if let Some(p) = &mut self.presenter {
                        p.set_occluded(occluded);
                    }
                }
                WindowEvent::RedrawRequested => self.redraw(),
                _ => {}
            }
        }

        fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {
            // `--burst-wait`: 3 s after the burst. Still deferred: winit
            // slept through the frame callback.
            if self.phase == Phase::Burst && self.burst_end.is_some() {
                self.report.burst_stalled += 1;
                self.report.fail(format!(
                    "burst {}: the deferred frame stayed off screen for 3 s",
                    self.report.burst_rounds
                ));
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.pump();
            if let Some(since) = self.settling {
                let presenter = self.presenter.as_mut().expect("presenter");
                if !presenter.has_deferred() && !presenter.frame_pending() {
                    self.settling = None;
                    self.settled();
                } else if since.elapsed() > Duration::from_secs(2) {
                    self.settling = None;
                    self.report
                        .fail(format!("{:?}: no frame callback within 2 s", self.phase));
                    self.settled();
                }
            }
            if let Some((tag, give_up)) = &self.shot
                && (shot_done(tag) || Instant::now() >= *give_up)
            {
                if !shot_done(tag) {
                    let msg = format!("screenshot {tag} never came");
                    self.report.fail(msg);
                }
                self.shot = None;
                self.enter(self.after_shot);
            }
            if let Some(at) = self.wake_at
                && Instant::now() >= at
            {
                self.wake_at = None;
                self.timer(event_loop);
            }
            if self.start.elapsed() > Duration::from_secs(80) {
                self.report.fail("timed out".to_string());
                event_loop.exit();
            }
            // Sleep until the next thing due. A deferred frame's deadline
            // is armed except under `--burst-wait`, which shows what
            // happens without it.
            let now = Instant::now();
            let mut next = self.wake_at;
            let mut due = |at: Instant| next = Some(next.map_or(at, |n| n.min(at)));
            let presenter = self.presenter.as_ref();
            if !(self.burst_wait && self.phase == Phase::Burst)
                && let Some(at) = presenter.and_then(CpuPresenter::pending_deadline)
            {
                due(at);
            }
            if let Some(since) = self.settling {
                due(since + Duration::from_secs(2));
            }
            if self.shot.is_some() {
                due(now + Duration::from_millis(50));
            }
            event_loop.set_control_flow(match next {
                Some(at) => ControlFlow::WaitUntil(at),
                None => ControlFlow::Wait,
            });
        }
    }
}
