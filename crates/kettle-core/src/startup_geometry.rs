//! Private, bounded diagnostics for the first native PTY. No terminal content.

use std::fmt::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use portable_pty::PtySize;

const TARGET: &str = "kettle::pty_geometry";
const MAX_EVENTS: usize = 64;
// A tail beyond the child's two-second observation. Consumers must verify
// actual child/end timestamps, rather than assuming this duration covers it.
const INTERVAL: Duration = Duration::from_secs(3);
static CLAIMED: AtomicBool = AtomicBool::new(false);

fn now_ns() -> u64 {
    #[cfg(target_os = "macos")]
    let clock = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_os = "macos"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the selected clock and writable timespec are valid.
    if unsafe { libc::clock_gettime(clock, &mut time) } != 0 {
        return 0;
    }
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

fn read_size(fd: Option<std::os::fd::RawFd>) -> Result<PtySize, i32> {
    let Some(fd) = fd else { return Err(-1) };
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd is borrowed from the live master, size is a valid output.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut size as *mut libc::winsize) } != 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
    }
    Ok(PtySize {
        rows: size.ws_row,
        cols: size.ws_col,
        pixel_width: size.ws_xpixel,
        pixel_height: size.ws_ypixel,
    })
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Outcome {
    Ok,
    Error,
    Noop,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Noop => "noop",
        }
    }
}

struct Event {
    t_ns: u64,
    requested: PtySize,
    observed: Result<PtySize, i32>,
    outcome: Outcome,
    native_error: Option<i32>,
}

pub(crate) struct Trace {
    launch_id: u32,
    pane_id: Option<u64>,
    child_pid: Option<u32>,
    start_ns: u64,
    created_ns: u64,
    initial_ns: u64,
    initial: Result<PtySize, i32>,
    deadline: Instant,
    events: Vec<Event>,
    dropped: u64,
    ended: bool,
}

impl Trace {
    pub(crate) fn begin() -> Option<u64> {
        if !log::log_enabled!(target: TARGET, log::Level::Info)
            || CLAIMED.swap(true, Ordering::Relaxed)
        {
            return None;
        }
        Some(now_ns())
    }

    pub(crate) fn start(
        fd: Option<std::os::fd::RawFd>,
        recording_start_ns: Option<u64>,
    ) -> Option<Self> {
        let start_ns = recording_start_ns?;
        let created_ns = now_ns();
        let initial = read_size(fd);
        Some(Self {
            launch_id: std::process::id(),
            pane_id: None,
            child_pid: None,
            start_ns,
            created_ns,
            initial_ns: now_ns(),
            initial,
            deadline: Instant::now() + INTERVAL,
            events: Vec::with_capacity(MAX_EVENTS),
            dropped: 0,
            ended: false,
        })
    }

    pub(crate) fn link(&mut self, pane_id: u64, child_pid: Option<u32>) {
        self.pane_id = Some(pane_id);
        self.child_pid = child_pid;
    }

    pub(crate) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub(crate) fn record(
        &mut self,
        fd: Option<std::os::fd::RawFd>,
        requested: PtySize,
        outcome: Outcome,
        native_error: Option<i32>,
    ) {
        self.push(Event {
            t_ns: now_ns(),
            requested,
            observed: read_size(fd),
            outcome,
            native_error,
        });
    }

    fn push(&mut self, event: Event) {
        if self.events.len() == MAX_EVENTS {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.events.push(event);
        }
    }

    pub(crate) fn finish(&mut self, fd: Option<std::os::fd::RawFd>, alive: bool) {
        let final_size = read_size(fd);
        let end_ns = now_ns();
        let complete = self.is_complete(end_ns, &final_size, alive);
        self.emit(end_ns, &final_size, complete);
    }

    fn is_complete(&self, end_ns: u64, final_size: &Result<PtySize, i32>, alive: bool) -> bool {
        alive
            && self.remaining().is_zero()
            && self.start_ns != 0
            && self.initial_ns >= self.start_ns
            && self.created_ns >= self.start_ns
            && end_ns >= self.initial_ns
            && self.pane_id.is_some()
            && self.child_pid.is_some()
            && self.initial.is_ok()
            && final_size.is_ok()
            && self.dropped == 0
            && self
                .events
                .iter()
                .all(|e| e.t_ns != 0 && e.observed.is_ok() && !matches!(e.outcome, Outcome::Error))
    }

    fn render(&self, end_ns: u64, final_size: &Result<PtySize, i32>, complete: bool) -> String {
        #[cfg(target_os = "macos")]
        let clock = "CLOCK_UPTIME_RAW";
        #[cfg(not(target_os = "macos"))]
        let clock = "CLOCK_MONOTONIC";
        let number = |n: Option<u64>| n.map_or_else(|| "null".to_owned(), |n| format!("\"{n}\""));
        let mut json = format!(
            "{{\"version\":\"native_pty_v1\",\"clock\":\"{clock}\",\"launch_id\":\"{}\",\"pane_id\":{},\"child_pid\":{},\"recording_start_ns\":{},\"created_ns\":{},\"initial_stage\":\"after_create_before_correction\",\"initial\":",
            self.launch_id,
            number(self.pane_id),
            self.child_pid
                .map_or_else(|| "null".to_owned(), |pid| pid.to_string()),
            self.start_ns,
            self.created_ns
        );
        geometry_json(&mut json, &self.initial, Some(self.initial_ns));
        json.push_str(",\"events\":[");
        for (i, event) in self.events.iter().enumerate() {
            if i != 0 {
                json.push(',');
            }
            let _ = write!(
                json,
                "{{\"seq\":{},\"t_ns\":{},\"reason\":\"resize\",\"launch_id\":\"{}\",\"pane_id\":{},\"signal_sent\":{},\"outcome\":\"{}\",\"native_error\":{},\"requested\":",
                i + 1,
                event.t_ns,
                self.launch_id,
                number(self.pane_id),
                if matches!(event.outcome, Outcome::Noop) {
                    "false"
                } else {
                    "null"
                },
                event.outcome.name(),
                event
                    .native_error
                    .map_or_else(|| "null".to_owned(), |error| error.to_string())
            );
            geometry_json(&mut json, &Ok(event.requested), None);
            json.push_str(",\"observed\":");
            geometry_json(&mut json, &event.observed, None);
            json.push('}');
        }
        let _ = write!(
            json,
            "],\"event_count\":{},\"recording_end_ns\":{end_ns},\"complete\":{complete},\"dropped\":{},\"overflow\":{},\"final\":",
            self.events.len(),
            self.dropped,
            self.dropped != 0
        );
        geometry_json(&mut json, final_size, Some(end_ns));
        json.push('}');
        json
    }

    fn emit(&mut self, end_ns: u64, final_size: &Result<PtySize, i32>, complete: bool) {
        if !self.ended {
            self.ended = true;
            log::info!(target: TARGET, "native_pty={}", self.render(end_ns, final_size, complete));
        }
    }
}

fn geometry_json(out: &mut String, size: &Result<PtySize, i32>, t_ns: Option<u64>) {
    out.push('{');
    if let Some(t_ns) = t_ns {
        let _ = write!(out, "\"t_ns\":{t_ns},");
    }
    match size {
        Ok(s) => {
            let _ = write!(
                out,
                "\"cols\":{},\"rows\":{},\"pixel_width\":{},\"pixel_height\":{},\"error\":null",
                s.cols, s.rows, s.pixel_width, s.pixel_height
            );
        }
        Err(error) => {
            let _ = write!(
                out,
                "\"cols\":null,\"rows\":null,\"pixel_width\":null,\"pixel_height\":null,\"error\":{error}"
            );
        }
    }
    out.push('}');
}

impl Drop for Trace {
    fn drop(&mut self) {
        // Constructor failures have no Terminal or final native handle yet.
        self.emit(now_ns(), &Err(-1), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace() -> Trace {
        Trace {
            launch_id: 7,
            pane_id: Some(1),
            child_pid: Some(8),
            start_ns: 10,
            created_ns: 10,
            initial_ns: 11,
            initial: Ok(PtySize {
                cols: 99,
                rows: 30,
                pixel_width: 792,
                pixel_height: 480,
            }),
            deadline: Instant::now(),
            events: Vec::with_capacity(MAX_EVENTS),
            dropped: 0,
            ended: true,
        }
    }

    #[test]
    fn incomplete_intervals_never_prove_zero_resize() {
        let mut trace = trace();
        let final_size = trace.initial;
        assert!(trace.is_complete(20, &final_size, true));
        assert!(!trace.is_complete(20, &final_size, false));
        assert!(!trace.is_complete(20, &Err(libc::EBADF), true));
        assert!(!trace.is_complete(0, &final_size, true));
        trace.start_ns = 0;
        assert!(!trace.is_complete(20, &final_size, true));
        trace.start_ns = 10;
        trace.initial_ns = 9;
        trace.created_ns = 10;
        assert!(!trace.is_complete(20, &final_size, true));
        trace.initial_ns = 11;
        trace.created_ns = 10;
        trace.deadline = Instant::now() + INTERVAL;
        assert!(!trace.is_complete(20, &final_size, true));
        trace.deadline = Instant::now();
        trace.pane_id = None;
        assert!(!trace.is_complete(20, &final_size, true));
        trace.pane_id = Some(1);
        trace.child_pid = None;
        assert!(!trace.is_complete(20, &final_size, true));
        trace.child_pid = Some(8);
        trace.initial = Err(libc::EBADF);
        assert!(!trace.is_complete(20, &final_size, true));
        trace.initial = final_size;
        trace.dropped = 1;
        assert!(!trace.is_complete(20, &final_size, true));
        trace.dropped = 0;
        trace.push(Event {
            t_ns: 12,
            requested: final_size.unwrap(),
            observed: final_size,
            outcome: Outcome::Error,
            native_error: Some(-1),
        });
        assert!(!trace.is_complete(20, &final_size, true));
        trace.events[0].outcome = Outcome::Noop;
        trace.events[0].observed = Err(libc::EBADF);
        assert!(!trace.is_complete(20, &final_size, true));
        trace.events[0].observed = final_size;
        trace.events[0].t_ns = 0;
        assert!(!trace.is_complete(20, &final_size, true));
    }

    #[test]
    fn recorder_wiring_covers_creation_noops_failures_and_drop() {
        let source = kettle_test_support::production_source(include_str!("term.rs"));
        let constructor = source
            .split_once("let pair = pty.openpty(native_pty_size(geometry))?;")
            .unwrap()
            .1;
        assert!(
            constructor
                .find("Trace::start(pair.master.as_raw_fd(), recording_start_ns)")
                .unwrap()
                < constructor.find("let mut cmd =").unwrap()
        );
        assert!(
            source.find("Trace::begin()").unwrap()
                < source
                    .find("let pair = pty.openpty(native_pty_size(geometry))?;")
                    .unwrap()
        );
        let resize = source
            .split_once("pub fn try_resize_geometry(")
            .unwrap()
            .1
            .split_once("\n    fn ")
            .unwrap()
            .0;
        let flat: String = resize.split_whitespace().collect();
        assert!(flat.contains("if!local_changed&&!resize_native{self.record_startup_resize(desired,false,&Ok(()));returnOk(());}"));
        assert!(
            flat.find("self.record_startup_resize(desired,resize_native,&native_result)")
                .unwrap()
                < flat.find("ifnative_result.is_ok()").unwrap()
        );
        let drop = source.split_once("impl Drop for Terminal {").unwrap().1;
        assert!(
            drop.find("trace.finish(").unwrap() < drop.find("self.drain_output.store(").unwrap()
        );
        let module = include_str!("startup_geometry.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        assert!(module.contains("log::log_enabled!(target: TARGET, log::Level::Info)"));
        assert!(!module.contains("std::env::var"));
        assert!(!module.contains("std::thread"));
        let mux = include_str!("../../kettle-ui/src/mux.rs")
            .split("#[cfg(test)]\nmod node_tests")
            .next()
            .unwrap();
        assert!(mux.contains("term.link_startup_geometry(id);"));
    }

    #[test]
    fn the_initial_observation_survives_a_correction() {
        let mut trace = trace();
        let corrected = PtySize {
            cols: 100,
            ..trace.initial.unwrap()
        };
        trace.push(Event {
            t_ns: 12,
            requested: corrected,
            observed: Ok(corrected),
            outcome: Outcome::Ok,
            native_error: None,
        });
        let json: serde_json::Value =
            serde_json::from_str(&trace.render(13, &Ok(corrected), true)).unwrap();
        assert_eq!(json["initial"]["cols"], 99);
        assert_eq!(json["events"][0]["observed"]["cols"], 100);
        assert_eq!(json["final"]["cols"], 100);
    }

    #[test]
    fn overflow_errors_and_noops_are_explicit_and_bounded() {
        let mut trace = trace();
        for i in 0..MAX_EVENTS + 2 {
            trace.push(Event {
                t_ns: 12 + i as u64,
                requested: trace.initial.unwrap(),
                observed: if i == 0 {
                    Err(libc::EBADF)
                } else {
                    trace.initial
                },
                outcome: if i == 0 {
                    Outcome::Error
                } else {
                    Outcome::Noop
                },
                native_error: if i == 0 { Some(-1) } else { None },
            });
        }
        let line = trace.render(100, &Err(libc::EBADF), false);
        assert!(line.len() < 24 * 1024);
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(json["events"].as_array().unwrap().len(), MAX_EVENTS);
        assert_eq!(json["events"][0]["outcome"], "error");
        assert_eq!(json["events"][0]["native_error"], -1);
        assert_eq!(json["events"][1]["outcome"], "noop");
        assert_eq!(json["dropped"], 2);
        assert_eq!(json["overflow"], true);
        assert_eq!(json["complete"], false);
        assert_eq!(json["final"]["cols"], serde_json::Value::Null);
    }

    #[test]
    fn production_capture_retains_the_real_pty_across_a_correction() {
        // Pass the opt-in timestamp directly. No process-global logger or
        // first-pane claim is changed, so concurrent tests cannot steal it.
        let recording_start_ns = Some(now_ns());
        let pair = portable_pty::native_pty_system()
            .openpty(PtySize {
                cols: 99,
                rows: 30,
                pixel_width: 792,
                pixel_height: 480,
            })
            .expect("this regression requires a real PTY");
        let fd = pair.master.as_raw_fd();
        let mut trace = Trace::start(fd, recording_start_ns).unwrap();
        trace.link(1, Some(8));
        let corrected = PtySize {
            cols: 100,
            rows: 30,
            pixel_width: 800,
            pixel_height: 480,
        };
        pair.master.resize(corrected).unwrap();
        trace.record(fd, corrected, Outcome::Ok, None);
        trace.deadline = Instant::now();
        trace.finish(fd, true);
        assert!(trace.ended);
        let end_ns = now_ns();
        let final_size = read_size(fd);
        assert!(trace.is_complete(end_ns, &final_size, true));
        let json: serde_json::Value =
            serde_json::from_str(&trace.render(end_ns, &final_size, true)).unwrap();
        assert_eq!(json["initial"]["cols"], 99);
        assert_eq!(json["initial"]["pixel_width"], 792);
        assert_eq!(json["events"][0]["observed"]["cols"], 100);
        assert_eq!(json["final"]["cols"], 100);
        assert_eq!(json["version"], "native_pty_v1");
        assert_eq!(json["launch_id"], std::process::id().to_string());
        assert_eq!(json["pane_id"], "1");
        assert_eq!(json["event_count"], 1);
        assert_eq!(json["events"][0]["reason"], "resize");
        assert_eq!(json["events"][0]["signal_sent"], serde_json::Value::Null);
        assert_eq!(json["events"][0]["pane_id"], "1");
        assert_eq!(json["final"]["t_ns"], end_ns);
    }

    #[test]
    fn native_observation_reads_the_kernel_before_a_correction() {
        let pty = portable_pty::native_pty_system();
        let pair = match pty.openpty(PtySize {
            cols: 99,
            rows: 30,
            pixel_width: 792,
            pixel_height: 480,
        }) {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("skipping: no PTY ({error})");
                return;
            }
        };
        let initial = read_size(pair.master.as_raw_fd()).unwrap();
        pair.master
            .resize(PtySize {
                cols: 100,
                ..initial
            })
            .unwrap();
        assert_eq!(initial.cols, 99);
        assert_eq!(read_size(pair.master.as_raw_fd()).unwrap().cols, 100);
        assert_eq!(read_size(None), Err(-1));
    }
}
