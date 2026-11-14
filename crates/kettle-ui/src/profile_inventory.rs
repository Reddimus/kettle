//! Cached profile discovery shared by every UI surface.

/// One worker, one pending invalidation and one latest result. Enumeration
/// never holds the result lock; dispatch only reads the event-loop-owned cache.
pub(crate) struct ProfileInventory {
    cached: Vec<String>,
    ready: bool,
    requests: Option<std::sync::mpsc::SyncSender<()>>,
    latest: std::sync::Arc<std::sync::Mutex<Option<std::io::Result<Vec<String>>>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ProfileInventory {
    pub(crate) fn new(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        Self::with_enumerator(kettle_config::Config::try_list_profiles, wake)
    }

    pub(crate) fn with_enumerator(
        enumerate: impl Fn() -> std::io::Result<Vec<String>> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let (requests, rx) = std::sync::mpsc::sync_channel(1);
        let latest = std::sync::Arc::new(std::sync::Mutex::new(None));
        let result = latest.clone();
        let worker = std::thread::Builder::new()
            .name("kettle-profiles".into())
            .spawn(move || {
                loop {
                    let profiles = enumerate();
                    if let Err(error) = &profiles {
                        log::warn!("profile inventory unavailable: {error}");
                    }
                    let notify = {
                        let mut latest = result.lock().unwrap_or_else(|e| e.into_inner());
                        // A later error must not erase an unconsumed success.
                        if matches!((&*latest, &profiles), (Some(Ok(_)), Err(_))) {
                            false
                        } else {
                            latest.replace(profiles).is_none()
                        }
                    };
                    if notify {
                        wake();
                    }
                    // Poll on the worker as well: profile creation/removal need not
                    // touch the active config file watched by live reload.
                    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            cached: Vec::new(),
            ready: false,
            requests: Some(requests),
            latest,
            worker: Some(worker),
        })
    }

    pub(crate) fn refresh(&self) {
        if let Some(requests) = &self.requests {
            let _ = requests.try_send(());
        }
    }

    pub(crate) fn complete(&mut self) -> bool {
        let result = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take();
        let profiles = match result {
            Some(Ok(profiles)) => profiles,
            Some(Err(_)) => return false,
            None => return false,
        };
        if !self.ready || profiles != self.cached {
            self.cached = profiles;
            self.ready = true;
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn profiles(&self) -> &[String] {
        &self.cached
    }

    /// None until the first successful completion; later scans retain the last snapshot.
    pub(crate) fn snapshot(&self) -> Option<&[String]> {
        self.ready.then_some(self.cached.as_slice())
    }
}

impl Drop for ProfileInventory {
    fn drop(&mut self) {
        self.requests.take();
        // A stalled filesystem must not stall event-loop shutdown either.
        self.worker.take();
    }
}

#[cfg(test)]
pub(crate) mod test_support;
