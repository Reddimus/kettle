//! The system fonts, loaded on their own thread while the event loop starts.
//!
//! Enumerating the system fonts is most of the font work before the first
//! pane can start, and none of it needs a display: only measuring the cell
//! needs the monitor's scale. `run_with` starts this thread before it builds
//! the event loop. Window 1 joins it before its pane spawns, before `run_app`
//! on eligible macOS launches and in `resumed` otherwise. When it fails, the
//! first window loads the fonts itself, as every later window does.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;

use kettle_config::Config;
use kettle_render::{PreparedFonts, StartupFonts};

pub(crate) struct FontPreload {
    /// Sends the configured family, once. Dropping it unsent lets the thread
    /// finish without waiting for a family.
    family: Option<SyncSender<String>>,
    thread: Option<JoinHandle<PreparedFonts>>,
}

impl FontPreload {
    /// Start enumerating the system fonts on the `kettle-font-preload`
    /// thread.
    pub(crate) fn start() -> Self {
        let (family, requested) = sync_channel(1);
        let thread = match std::thread::Builder::new()
            .name("kettle-font-preload".into())
            .spawn(move || prepare(&requested))
        {
            Ok(thread) => Some(thread),
            Err(error) => {
                log::warn!("font preload thread unavailable: {error}");
                None
            }
        };
        Self {
            family: Some(family),
            thread,
        }
    }

    /// Tell the thread which family the config chose, so it loads that
    /// family's faces before the first window measures them. Only the first
    /// call counts.
    pub(crate) fn request_family(&mut self, family: &str) {
        if let Some(sender) = self.family.take() {
            // Fails only if the thread never started, and then `finish`
            // loads the fonts itself.
            let _ = sender.try_send(family.to_owned());
        }
    }

    /// The fonts for `cfg` at `scale`, the display's scale factor. Waits for
    /// the thread if it is still loading them.
    pub(crate) fn finish(mut self, cfg: &Config, scale: f32) -> StartupFonts {
        self.request_family(&cfg.font_family);
        crate::startup_trace::mark(crate::startup_trace::Phase::FontsJoinStart);
        let joined = self.thread.take().and_then(|thread| thread.join().ok());
        // Without the thread's fonts, enumerate and measure them inside the
        // stamped wait, including the cold family matches and face loading.
        let prepared = joined.unwrap_or_else(PreparedFonts::enumerate);
        let fonts = prepared.measure(cfg, scale);
        crate::startup_trace::mark(crate::startup_trace::Phase::FontsJoined);
        fonts
    }
}

/// The preload thread's work.
fn prepare(requested: &Receiver<String>) -> PreparedFonts {
    raise_priority();
    let mut prepared = PreparedFonts::enumerate();
    crate::startup_trace::mark(crate::startup_trace::Phase::FontsEnumerated);
    // The config is read after the event loop is built, usually long after
    // the enumeration ends. Until it arrives, warm the compiled-in family,
    // which a config without `font-family` uses. A preload dropped before
    // then closes the channel, and the thread ends.
    let family = match requested.try_recv() {
        Ok(family) => Some(family),
        Err(TryRecvError::Empty) => {
            prepared.warm_family(kettle_config::font::FAMILY);
            requested.recv().ok()
        }
        Err(TryRecvError::Disconnected) => None,
    };
    if let Some(family) = family {
        // Does nothing when the config kept the compiled-in family.
        prepared.warm_family(&family);
    }
    crate::startup_trace::mark(crate::startup_trace::Phase::FontsReady);
    prepared
}

/// Run at the class of work the user is waiting for. On macOS a thread
/// spawned from the main thread starts at the default class, below the main
/// thread's, and the main thread joining it does not raise it.
fn raise_priority() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: sets the calling thread's own QoS class; no pointers.
        let status = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
        };
        if status != 0 {
            log::debug!("font preload thread keeps its QoS class: error {status}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FontPreload;
    use kettle_config::Config;
    use kettle_render::StartupFonts;
    use std::time::{Duration, Instant};

    /// The preloaded fonts measure what the first window would have loaded
    /// itself, whether the family was requested early or only at the end.
    #[test]
    fn finish_measures_what_a_direct_load_measures() {
        let default = Config::default();
        let other = Config {
            font_family: "Kettle Test Family That Is Not Installed".to_owned(),
            font_size: 17.0,
            ..Config::default()
        };
        for (cfg, scale, request_early) in [(&default, 1.0, true), (&other, 2.0, false)] {
            let mut preload = FontPreload::start();
            assert!(preload.thread.is_some(), "the preload thread started");
            if request_early {
                preload.request_family(&cfg.font_family);
            }
            let fonts = preload.finish(cfg, scale);
            assert!(fonts.matches(cfg, scale));
            assert_eq!(fonts.cell, StartupFonts::load(cfg, scale).cell);
        }
    }

    /// Without a thread, the first window loads the fonts itself.
    #[test]
    fn finish_without_a_thread_loads_the_fonts_directly() {
        let cfg = Config::default();
        let preload = FontPreload {
            family: None,
            thread: None,
        };
        let fonts = preload.finish(&cfg, 2.0);
        assert!(fonts.matches(&cfg, 2.0));
        assert_eq!(fonts.cell, StartupFonts::load(&cfg, 2.0).cell);
    }

    /// A startup that ends before its first window drops the preload
    /// unfinished. The thread must not wait for a family forever.
    #[test]
    fn a_preload_dropped_unfinished_ends_its_thread() {
        let FontPreload { family, thread } = FontPreload::start();
        let thread = thread.expect("the preload thread started");
        drop(family);
        // Generous: an unoptimized build enumerating a large font directory.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !thread.is_finished() {
            assert!(
                Instant::now() < deadline,
                "the preload thread still waits for a family"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(thread.join().is_ok());
    }

    /// The first window waits on this thread, so it must not rank below
    /// other work on a busy Mac.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_preload_thread_runs_at_user_initiated_qos() {
        fn class() -> u32 {
            let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
            let mut relative = 0;
            // SAFETY: both out-pointers are valid for the call, and the
            // thread is the caller's own.
            unsafe {
                libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut relative)
            };
            class as u32
        }
        // A closed channel: the thread enumerates and returns without a family.
        let (family, requested) = std::sync::mpsc::sync_channel::<String>(1);
        drop(family);
        let class = std::thread::spawn(move || {
            let _ = super::prepare(&requested);
            class()
        })
        .join()
        .expect("prepare ran");
        assert_eq!(class, libc::qos_class_t::QOS_CLASS_USER_INITIATED as u32);
    }
}
