//! One process-wide adapter walk, isolated from the event loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};

type Choices = Vec<(String, String)>;

#[derive(Clone, Default)]
pub(crate) struct GpuDiscovery(Arc<DiscoveryState>);

#[derive(Default)]
struct DiscoveryState {
    started: AtomicBool,
    choices: OnceLock<Choices>,
}

pub(crate) static GPU_DISCOVERY: LazyLock<GpuDiscovery> = LazyLock::new(GpuDiscovery::default);

pub(crate) fn detect_choices() -> Choices {
    kettle_render::detect_gpus()
        .into_iter()
        .map(|g| {
            (
                format!("{:x}:{:x}:{}", g.vendor, g.device, g.name),
                format!("{} ({})", g.name, g.kind),
            )
        })
        .collect()
}

impl GpuDiscovery {
    pub(crate) fn cached(&self) -> Option<&Choices> {
        self.0.choices.get()
    }

    pub(crate) fn request(
        &self,
        discover: impl FnOnce() -> Choices + Send + 'static,
        complete: impl FnOnce(Choices) + Send + 'static,
    ) {
        if self.0.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let state = self.0.clone();
        // Retain completion on spawn failure too, so Settings can leave its pending state.
        let completion = Arc::new(std::sync::Mutex::new(Some(complete)));
        let worker_completion = completion.clone();
        let result = std::thread::Builder::new()
            .name("kettle-gpu-discovery".into())
            .spawn(move || {
                let choices = std::panic::catch_unwind(std::panic::AssertUnwindSafe(discover))
                    .unwrap_or_else(|_| {
                        log::warn!("GPU discovery panicked");
                        Vec::new()
                    });
                let _ = state.choices.set(choices.clone());
                if let Some(complete) = worker_completion.lock().unwrap().take() {
                    complete(choices);
                }
            });
        if let Err(error) = result {
            log::warn!("GPU discovery worker failed to start: {error}");
            let _ = self.0.choices.set(Vec::new());
            if let Some(complete) = completion.lock().unwrap().take() {
                complete(Vec::new());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_delivers_and_caches_adapter_choices() {
        let discovery = GpuDiscovery::default();
        let choices = vec![(
            "1234:5678:adapter".to_string(),
            "adapter (Integrated)".to_string(),
        )];
        let expected = choices.clone();
        let (send, receive) = std::sync::mpsc::channel();
        discovery.request(move || choices, move |result| send.send(result).unwrap());
        assert_eq!(
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            expected
        );
        assert_eq!(discovery.cached(), Some(&expected));
    }

    #[test]
    fn discovery_returns_while_driver_is_blocked_and_caches_empty_result() {
        let discovery = GpuDiscovery::default();
        let (release, blocked) = std::sync::mpsc::channel();
        let (done, results) = std::sync::mpsc::channel();
        let caller = std::thread::current().id();
        discovery.request(
            move || {
                assert_ne!(caller, std::thread::current().id());
                blocked.recv().unwrap();
                Vec::new()
            },
            move |choices| done.send(choices).unwrap(),
        );
        assert!(discovery.cached().is_none());
        discovery.request(
            || panic!("discovery must only run once"),
            |_| panic!("duplicate completion"),
        );
        release.send(()).unwrap();
        assert!(
            results
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .is_empty()
        );
        assert_eq!(discovery.cached(), Some(&Vec::new()));
        discovery.request(
            || panic!("empty result must be cached"),
            |_| panic!("duplicate completion"),
        );
    }
}
