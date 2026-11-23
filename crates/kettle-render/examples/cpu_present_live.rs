//! Live check of `kettle_render::cpu_present` on a real window (Linux).
//!
//! Opens a winit window and, with no GPU code, presents frames through the
//! CPU presenter: a moving square with exact damage, opaque and translucent
//! frames in turn, resizes, a flood of presents faster than the compositor's
//! frame clock, a forced second buffer, idle release and occlusion. Each
//! frame repaints only the region the presenter asks for and is then checked
//! against a full reference frame, so a wrong buffer age shows up as a
//! mismatch. It prints one `CPU_PRESENT_RESULT {json}` line and exits 1 if
//! any check failed.
//!
//! ```sh
//! cargo run -p kettle-render --example cpu_present_live [-- --softbuffer] [--size WxH] [--maximize]
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
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId};

    const SQUARE: u32 = 40;
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
        let event_loop = EventLoop::new().expect("event loop");
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
            report: Report::default(),
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
            PixelFormat::Opaque => 0xff20_3040,
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
                 \"allocations\":{},\"failures\":{}",
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
                PresentOutcome::Presented => self.report.presented += 1,
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

        fn redraw(&mut self, event_loop: &ActiveEventLoop) {
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
                        event_loop
                            .set_control_flow(ControlFlow::WaitUntil(self.wake_at.expect("wake")));
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
                    self.next_phase(Phase::Done);
                    self.wake_at = Some(Instant::now() + Duration::from_millis(300));
                    event_loop
                        .set_control_flow(ControlFlow::WaitUntil(self.wake_at.expect("wake")));
                }
                Phase::Done => {}
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
                    event_loop.set_control_flow(ControlFlow::Wait);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                Phase::Done => {
                    if let Some(p) = &self.presenter {
                        self.report.allocations = p.stats().allocations;
                    }
                    event_loop.exit();
                }
                _ => {}
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
                WindowEvent::RedrawRequested => self.redraw(event_loop),
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if let Some(at) = self.wake_at
                && Instant::now() >= at
            {
                self.wake_at = None;
                self.timer(event_loop);
            }
            if self.start.elapsed() > Duration::from_secs(60) {
                self.report.fail("timed out".to_string());
                event_loop.exit();
            }
        }
    }
}
