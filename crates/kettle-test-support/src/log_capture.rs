//! The one log capture a test binary installs.
//!
//! `log` accepts a single logger per process, so every test that reads log
//! output shares this one; a second private logger would lose the install race
//! and see nothing.

use std::cell::RefCell;
use std::sync::Once;

thread_local! {
    /// The `(target, line)` pairs logged on this thread while a
    /// [`lines_logged`] call is running on it, and `None` otherwise.
    static LINES: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
}

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        // `try_with`: a destructor that logs can run after this thread's
        // `LINES` is gone, and panicking there would abort the test binary.
        let recording = LINES.try_with(|lines| lines.borrow().is_some());
        if recording != Ok(true) {
            return;
        }
        // Format before borrowing: an argument's `Display` may log too.
        let line = (record.target().to_owned(), record.args().to_string());
        let _ = LINES.try_with(|lines| {
            if let Some(lines) = lines.borrow_mut().as_mut() {
                lines.push(line);
            }
        });
    }
    fn flush(&self) {}
}

/// Puts back what the thread was recording before, also when `run` panics.
struct Restore(Option<Vec<(String, String)>>);

impl Drop for Restore {
    fn drop(&mut self) {
        let outer = self.0.take();
        let _ = LINES.try_with(|lines| *lines.borrow_mut() = outer);
    }
}

/// What `run` returned, with the lines it logged on this thread from the
/// targets `wanted` accepts. Only the calling thread is recorded, so
/// concurrent tests do not see each other's lines and the rest of the test
/// binary logs into nothing.
///
/// # Panics
///
/// Panics when the test binary already installed another logger.
pub fn lines_logged<T>(wanted: impl Fn(&str) -> bool, run: impl FnOnce() -> T) -> (T, Vec<String>) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        log::set_logger(&Capture).expect("no other logger in this test binary");
        log::set_max_level(log::LevelFilter::Trace);
    });
    let restore = Restore(LINES.with(|lines| lines.borrow_mut().replace(Vec::new())));
    let value = run();
    let lines = LINES
        .with(|lines| lines.borrow_mut().take())
        .unwrap_or_default();
    drop(restore);
    let lines = lines
        .into_iter()
        .filter(|(target, _)| wanted(target))
        .map(|(_, line)| line)
        .collect();
    (value, lines)
}

#[cfg(test)]
mod tests {
    use super::lines_logged;

    #[test]
    fn only_the_calling_thread_and_the_wanted_targets_are_returned() {
        let (value, lines) = lines_logged(
            |target| target == "wanted",
            || {
                log::warn!(target: "wanted", "first");
                log::warn!(target: "other", "not this");
                std::thread::spawn(|| log::warn!(target: "wanted", "another thread"))
                    .join()
                    .unwrap();
                let (_, inner) = lines_logged(|_| true, || log::info!(target: "wanted", "nested"));
                assert_eq!(inner, ["nested"]);
                log::trace!(target: "wanted", "second");
                7
            },
        );
        assert_eq!(value, 7);
        assert_eq!(lines, ["first", "second"]);
        let (_, lines) = lines_logged(|_| true, || ());
        assert!(lines.is_empty());
    }
}
